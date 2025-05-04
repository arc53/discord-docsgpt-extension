# Discord DocsGPT Extension Bot

This is a Discord bot for DocsGPT to answer user questions within Discord. It can respond to direct messages (DMs) and mentions in server channels.

## Features

*   **Question Answering:** Answers questions using an external API endpoint.
*   **Conversation History:** Maintains conversation context for follow-up questions.
*   **Storage Options:** Supports both in-memory storage (lost on restart) and MongoDB for persistent chat history.
*   **Interaction Modes:** Responds to Direct Messages (DMs) and mentions (`@BotName question...`).
*   **Docker Support:** Includes a Dockerfile for easy containerization and deployment.
*   **Multi-Arch Builds:** GitHub Actions workflow builds Docker images for `linux/amd64` and `linux/arm64`.

## Prerequisites

*   Python 3.10+
*   A Discord Bot Token
*   An API Key for the backend service
*   (Optional) MongoDB connection URI if using MongoDB for storage

## Setup

1.  **Clone the repository:**
    ```bash
    git clone <your-repository-url>
    cd discord-docsgpt-extension
    ```

2.  **Install dependencies:**
    ```bash
    pip install -r requirements.txt
    ```

3.  **Create a `.env` file:**
    Copy the example below and fill in your actual credentials.
    ```dotenv
    # .env file
    DISCORD_TOKEN=YOUR_DISCORD_BOT_TOKEN
    API_KEY=YOUR_BACKEND_API_KEY

    # Optional: Base URL for the API if different from default
    # API_BASE=https://your-api-base-url.com

    # Optional: Storage configuration (defaults to 'memory')
    # STORAGE_TYPE=mongodb
    # MONGODB_URI=your_mongodb_connection_string
    # MONGODB_DB_NAME=discord_bot_db
    # MONGODB_COLLECTION_NAME=chat_histories
    ```

## Running the Bot

### Directly with Python

```bash
python bot.py
```

### Using Docker

1.  **Build the Docker image:**
    ```bash
    docker build -t discord-docsgpt-extension .
    ```

2.  **Run the Docker container:**
    Make sure your `.env` file is present in the directory where you run this command.
    ```bash
    docker run --env-file .env --rm -it discord-docsgpt-extension
    ```
    *   `--env-file .env`: Loads environment variables from the `.env` file.
    *   `--rm`: Removes the container when it exits.
    *   `-it`: Runs in interactive mode (allows you to see logs and stop with Ctrl+C).

## Configuration

The bot is configured using environment variables, typically stored in a `.env` file:

*   `DISCORD_TOKEN` (Required): Your Discord bot token.
*   `API_KEY` (Required): Your API key for the backend service.
*   `API_BASE` (Optional): The base URL for the backend API. Defaults to `https://gptcloud.arc53.com`.
*   `STORAGE_TYPE` (Optional): How to store conversation history. Options:
    *   `memory` (Default): Stores history in memory, lost on restart.
    *   `mongodb`: Stores history in a MongoDB database. Requires `MONGODB_URI`.
*   `MONGODB_URI` (Required if `STORAGE_TYPE=mongodb`): The connection string for your MongoDB instance.
*   `MONGODB_DB_NAME` (Optional, used with `mongodb`): The name of the database to use. Defaults to `discord_bot_memory`.
*   `MONGODB_COLLECTION_NAME` (Optional, used with `mongodb`): The name of the collection to store histories. Defaults to `chat_histories`.

## Usage

1.  **Direct Message (DM):** Send a message directly to the bot.
2.  **Mention:** Mention the bot at the beginning of your message in a server channel where it's present: `@YourBotName How do I use feature X?`

The bot will process your question, potentially using previous messages in the same DM channel or thread for context, query the backend API, and send back the answer.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request or open an issue.

## License
This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## Acknowledgments
- [DocsGPT](https://www.docsgpt.cloud/)
- [DocsGPT Github](https://github.com/arc53/docsgpt)
- [Discord.py](https://discordpy.readthedocs.io/en/stable/)
- [MongoDB](https://www.mongodb.com/)
