import os
import re
import logging
import aiohttp
import discord
from discord.ext import commands
import dotenv
import json
import datetime
from motor.motor_asyncio import AsyncIOMotorClient # Use motor for async MongoDB
from pymongo.errors import ConnectionFailure, ConfigurationError

dotenv.load_dotenv()

# --- Logging Configuration ---
logging.basicConfig(
    format="%(asctime)s - %(name)s - %(levelname)s - %(message)s", level=logging.INFO)
logging.getLogger("aiohttp").setLevel(logging.WARNING) # Optional: Quieter aiohttp logs
logger = logging.getLogger(__name__)

# --- Bot Configuration ---
TOKEN = os.getenv("DISCORD_TOKEN")
PREFIX = '!'  # Command prefix (can be adjusted or removed if only mention/DM based)
BASE_API_URL = os.getenv("API_BASE", "https://gptcloud.arc53.com")
API_URL = BASE_API_URL + "/api/answer"
API_KEY = os.getenv("API_KEY")

# --- Storage Configuration ---
STORAGE_TYPE = os.getenv("STORAGE_TYPE", "memory").lower() # Default to in-memory
MONGODB_URI = os.getenv("MONGODB_URI") # Required if STORAGE_TYPE is 'mongodb'
MONGODB_DB_NAME = os.getenv("MONGODB_DB_NAME", "discord_bot_memory")
MONGODB_COLLECTION_NAME = os.getenv("MONGODB_COLLECTION_NAME", "chat_histories")

# --- Global Storage Variables ---
mongo_client = None
mongo_collection = None
in_memory_storage = {} # Used if STORAGE_TYPE is 'memory'

# --- Initialize Storage ---
# Needs to be done asynchronously, ideally within an async function or bot setup hook
async def initialize_storage():
    global STORAGE_TYPE, mongo_client, mongo_collection
    if STORAGE_TYPE == "mongodb":
        if not MONGODB_URI:
            logger.error("STORAGE_TYPE is 'mongodb' but MONGODB_URI is not set. Exiting.")
            # In a real app, you might exit or fallback more gracefully
            exit(1)
        try:
            logger.info(f"Attempting to connect to MongoDB: {MONGODB_URI[:15]}... DB: {MONGODB_DB_NAME}")
            mongo_client = AsyncIOMotorClient(MONGODB_URI, serverSelectionTimeoutMS=5000)
            # The ismaster command is cheap and does not require auth.
            await mongo_client.admin.command('ismaster')
            db = mongo_client[MONGODB_DB_NAME]
            mongo_collection = db[MONGODB_COLLECTION_NAME]
            logger.info(f"Successfully connected to MongoDB and selected collection '{MONGODB_COLLECTION_NAME}'.")
        except (ConnectionFailure, ConfigurationError) as e:
            logger.error(f"Failed to connect to MongoDB: {e}", exc_info=True)
            logger.warning("Falling back to in-memory storage due to MongoDB connection error.")
            STORAGE_TYPE = "memory"
            mongo_client = None
            mongo_collection = None
        except Exception as e:
            logger.error(f"An unexpected error occurred during MongoDB initialization: {e}", exc_info=True)
            logger.warning("Falling back to in-memory storage.")
            STORAGE_TYPE = "memory"
            mongo_client = None
            mongo_collection = None
    elif STORAGE_TYPE == "memory":
        logger.info("Using in-memory storage for chat history (will be lost on restart).")
    else:
        logger.warning(f"Unknown STORAGE_TYPE '{STORAGE_TYPE}'. Defaulting to in-memory storage.")
        STORAGE_TYPE = "memory"

# --- Storage Access Functions ---

async def get_user_data(user_id: int) -> dict:
    """
    Fetches chat history, conversation ID, and user info from the configured storage
    based on the Discord User ID.
    """
    user_id_str = str(user_id)
    # Default structure includes user_info
    default_data = {"history": [], "conversation_id": None, "user_info": None}

    if STORAGE_TYPE == "mongodb" and mongo_collection is not None:
        try:
            # Using user_id as the document ID (_id)
            doc = await mongo_collection.find_one({"_id": user_id_str})
            if doc:
                # Use .get for safety, defaulting to empty/None
                history = doc.get("conversation_history", [])
                conv_id = doc.get("conversation_id", None)
                user_info = doc.get("user_info", None)
                return {"history": history, "conversation_id": conv_id, "user_info": user_info}
            else:
                return default_data
        except Exception as e:
            logger.error(f"MongoDB Error fetching data for user_id {user_id_str}: {e}", exc_info=True)
            return default_data # Return default on error
    else: # In-memory storage
        data = in_memory_storage.get(user_id_str, default_data)
        # Ensure all keys exist, using .get with defaults
        return {
            "history": data.get("history", []),
            "conversation_id": data.get("conversation_id", None),
            "user_info": data.get("user_info", None)
        }

async def save_user_data(user_id: int, history: list, conversation_id: str | None, user_info: dict | None):
    """
    Saves chat history, conversation ID, and user info to the configured storage
    based on the Discord User ID. Limits history length.
    """
    user_id_str = str(user_id)
    max_history_len_pairs = 10 # Store last 10 pairs (user + assistant = 1 pair = 2 messages)
    limited_history = history[-(max_history_len_pairs * 2):]

    if STORAGE_TYPE == "mongodb" and mongo_collection is not None:
        try:
            update_data = {
                "conversation_history": limited_history,
                "conversation_id": conversation_id,
            }
            if user_info:
                 # Filter out potential non-serializable discord objects if any slip through
                serializable_user_info = {k: v for k, v in user_info.items() if isinstance(v, (str, int, float, bool, list, dict, type(None)))}
                update_data["user_info"] = serializable_user_info

            update_doc = {
                "$set": update_data,
                "$currentDate": {"last_updated": True} # Track last update time
            }
            await mongo_collection.update_one(
                {"_id": user_id_str},
                update_doc,
                upsert=True # Create document if it doesn't exist
            )
            logger.debug(f"Saved MongoDB data for user {user_id_str} with user_info: {bool(user_info)}")
        except Exception as e:
            logger.error(f"MongoDB Error saving data for user_id {user_id_str}: {e}", exc_info=True)
    else: # In-memory storage
        if user_id_str not in in_memory_storage:
             in_memory_storage[user_id_str] = {} # Initialize if new user

        in_memory_storage[user_id_str].update({ # Use update to merge data
            "history": limited_history,
            "conversation_id": conversation_id,
            "last_updated": datetime.datetime.now(datetime.timezone.utc) # Timestamp
        })
        if user_info:
             serializable_user_info = {k: v for k, v in user_info.items() if isinstance(v, (str, int, float, bool, list, dict, type(None)))}
             in_memory_storage[user_id_str]["user_info"] = serializable_user_info
        logger.debug(f"Saved in-memory data for user {user_id_str} with user_info: {bool(user_info)}")


# --- Discord Bot Setup ---
intents = discord.Intents.default()
intents.message_content = True
intents.dm_messages = True # Ensure DM intents are enabled

bot = commands.Bot(command_prefix=PREFIX, intents=intents)

# Remove the old global dictionary
# conversation_histories = {}

# --- Helper Functions ---

def chunk_string(text, max_length=2000):
    """Splits a string into chunks suitable for Discord messages."""
    chunks = []
    while len(text) > max_length:
        # Find the last newline or space within the limit
        split_index = -1
        for char in ('\n', ' '):
            try:
                idx = text.rindex(char, 0, max_length)
                split_index = max(split_index, idx)
            except ValueError:
                continue

        if split_index == -1: # No natural break found, force break
            split_index = max_length

        chunks.append(text[:split_index])
        text = text[split_index:].lstrip() # Remove leading space from next chunk

    if text: # Append the remaining part
        chunks.append(text)
    return chunks

# The escape_markdown function might be less necessary if the API provides clean text
# or if you want to allow some markdown from the API. Keep it if needed.
# def escape_markdown(text): ...

def format_history_for_api(messages: list) -> list:
    """
    Converts internal history format [{'role': 'user', 'content': '...'}, ...]
    to the API required format [{'prompt': '...', 'response': '...'}, ...].
    Ensures only valid pairs are included.
    """
    api_history = []
    i = 0
    while i < len(messages):
        # Look for a user message
        if messages[i].get("role") == "user" and "content" in messages[i]:
            prompt_content = messages[i]["content"]
            response_content = None
            # Check if the next message is a corresponding assistant response
            if i + 1 < len(messages) and messages[i+1].get("role") == "assistant" and "content" in messages[i+1]:
                response_content = messages[i+1]["content"]
                # Add the pair to history
                api_history.append({"prompt": prompt_content, "response": response_content})
                i += 2 # Move past both user and assistant message
            else:
                # If there's a user message without a following assistant response
                # (e.g., the current turn), we don't include it as a pair for the API history.
                i += 1 # Move past the user message only
        else:
             # Skip messages not conforming to the role structure (e.g., system message if added later)
            i += 1
    return api_history


# --- API Interaction ---

async def generate_answer(question: str, messages: list, conversation_id: str | None) -> dict:
    """Generates an answer using the external API, handling history formatting and errors."""
    if not API_KEY:
        logger.warning("API_KEY is not set. Cannot call backend API.")
        return {"answer": "Error: Backend API key is not configured.", "conversation_id": conversation_id}

    # Format history *before* sending to API
    try:
        formatted_history = format_history_for_api(messages)
        # The API expects history as a JSON *string*
        history_json_string = json.dumps(formatted_history)
    except TypeError as e:
        logger.error(f"Failed to serialize history to JSON: {e}. History: {messages}", exc_info=True)
        history_json_string = "[]" # Send empty list on serialization error

    payload = {
        "question": question,
        "api_key": API_KEY,
        "history": history_json_string, # Pass the JSON string
        "conversation_id": conversation_id
    }
    headers = {
        "Content-Type": "application/json; charset=utf-8"
    }
    timeout = aiohttp.ClientTimeout(total=120) # Increased timeout
    default_error_msg = "Sorry, I couldn't get an answer from the backend service."

    try:
        async with aiohttp.ClientSession(timeout=timeout) as session:
            async with session.post(API_URL, json=payload, headers=headers) as resp:
                # Check for non-200 status codes
                if resp.status != 200:
                    error_details = f"Status {resp.status}"
                    try:
                        error_body = await resp.json()
                        error_details += f" - {error_body.get('detail', await resp.text())}"
                    except (json.JSONDecodeError, aiohttp.ContentTypeError):
                         error_details += f" - {await resp.text()}"
                    logger.error(f"API error: {error_details}")
                    # Return a specific error message if possible
                    return {"answer": f"{default_error_msg} (Error: {resp.status})", "conversation_id": conversation_id}

                # Process successful response
                data = await resp.json()
                answer = data.get("answer", default_error_msg)
                returned_conv_id = data.get("conversation_id", conversation_id) # Use returned ID
                return {"answer": answer, "conversation_id": returned_conv_id}

    except aiohttp.ClientConnectorError as e:
        logger.error(f"Network connection error calling API: {e}")
        return {"answer": f"{default_error_msg} (Network Error)", "conversation_id": conversation_id}
    except aiohttp.ClientError as e: # Catch other aiohttp client errors
        logger.error(f"Client error calling API: {e}", exc_info=True)
        return {"answer": f"{default_error_msg} (Client Error)", "conversation_id": conversation_id}
    except json.JSONDecodeError as e:
        # This might happen if the successful response wasn't valid JSON
        logger.error(f"Failed to decode JSON response from API: {e}. Response text: {await resp.text()}", exc_info=True)
        return {"answer": f"{default_error_msg} (Invalid Response Format)", "conversation_id": conversation_id}
    except Exception as e: # Catch any other unexpected errors
        logger.error(f"Unexpected error in generate_answer: {e}", exc_info=True)
        return {"answer": f"{default_error_msg} (Unexpected Error)", "conversation_id": conversation_id}


# --- Bot Events and Commands ---

@bot.event
async def on_ready():
    # Initialize storage when the bot is ready
    await initialize_storage()
    logger.info(f'{bot.user.name} has connected to Discord!')
    logger.info(f"Using storage type: {STORAGE_TYPE}")
    if STORAGE_TYPE == "mongodb":
        logger.info(f"Connected to MongoDB: {MONGODB_DB_NAME}/{MONGODB_COLLECTION_NAME}")
    if not API_KEY:
        logger.warning("API_KEY environment variable is not set! API calls will fail.")

@bot.command(name="start")
async def start_command(ctx):
    """Handles the !start command."""
    # Optionally clear history on /start
    # await save_user_data(ctx.author.id, [], None, None) # Example: Clear history
    await ctx.send(f"Hi {ctx.author.mention}! How can I assist you today?")


@bot.event
async def on_message(message: discord.Message):
    # Ignore messages from the bot itself
    if message.author == bot.user:
        return

    # Check if the message is a command invocation
    # Pass message to command processor first. If it's a command, it will be handled.
    ctx = await bot.get_context(message)
    if ctx.valid:
        await bot.process_commands(message)
        return # It was a command, stop processing here

    # Now, handle non-command messages (DMs and mentions)
    should_process = False
    content = message.content.strip()

    # 1. Check for Direct Message
    if isinstance(message.channel, discord.DMChannel):
        should_process = True
        logger.info(f"Received DM from user {message.author.id} ({message.author.name})")

    # 2. Check for mention at the start in Guild Channels
    elif message.guild:
        mention_pattern = re.compile(rf'^<@!?{bot.user.id}>\s*') # Matches <@bot_id> or <@!bot_id>
        match = mention_pattern.match(content)
        if match:
            should_process = True
            content = content[match.end():].strip() # Remove mention from content
            logger.info(f"Received mention in guild {message.guild.id}, channel {message.channel.id} from user {message.author.id} ({message.author.name})")

    # If neither DM nor mention, ignore the message
    if not should_process:
        return
    
    # Prevent processing empty messages after removing mention
    if not content:
        await message.channel.send("Please provide a question after mentioning me.")
        return

    user_id = message.author.id
    question = content

    # Extract serializable user info
    user_info_dict = {
        "id": message.author.id,
        "name": message.author.name,
        "discriminator": message.author.discriminator, # e.g. #1234
        "display_name": message.author.display_name, # Nickname in guild, or username
        "is_bot": message.author.bot,
        # Avoid storing complex objects like roles, guild, etc. directly
    }
    # Filter out None values if desired, although MongoDB handles None
    user_info_dict = {k: v for k, v in user_info_dict.items() if v is not None}


    # --- Get History and Call API ---
    async with message.channel.typing(): # Show "Bot is typing..." indicator
        user_data = await get_user_data(user_id)
        current_history = user_data["history"]
        current_conversation_id = user_data["conversation_id"]

        # Add user's message to internal history
        current_history.append({"role": "user", "content": question})

        # Generate the answer
        response_doc = await generate_answer(
            question,
            current_history, # Pass the history including the latest user message
            current_conversation_id
        )
        answer = response_doc["answer"]
        new_conversation_id = response_doc["conversation_id"] # Use the potentially updated ID

        # Add bot's response to internal history
        current_history.append({"role": "assistant", "content": answer})

        # Save updated data
        await save_user_data(user_id, current_history, new_conversation_id, user_info_dict)

    # --- Send Response ---
    answer_chunks = chunk_string(answer)
    for chunk in answer_chunks:
        try:
            await message.channel.send(chunk)
        except discord.Forbidden:
            logger.warning(f"Missing permissions to send message in channel {message.channel.id} (Guild: {message.guild.id if message.guild else 'DM'})")
            # Optionally notify user in DMs if possible, or log and give up
            break # Stop trying to send chunks if one fails due to permissions
        except discord.HTTPException as e:
            logger.error(f"Failed to send message chunk to {message.channel.id}: {e}", exc_info=True)
            # Potentially retry or break



if not TOKEN:
    logger.critical("DISCORD_TOKEN environment variable not set! Exiting.")
else:
    try:
        # Running the bot automatically starts the event loop where on_ready runs
        bot.run(TOKEN)
    except discord.LoginFailure:
        logger.critical("Failed to login with the provided Discord token. Check the token.")
    except Exception as e:
        logger.critical(f"An error occurred while running the bot: {e}", exc_info=True)
    finally:
        # Cleanup resources if needed, e.g., close MongoDB client cleanly
        if mongo_client:
                mongo_client.close()
                logger.info("MongoDB client closed.")