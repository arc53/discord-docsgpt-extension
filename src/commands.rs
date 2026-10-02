//! Slash commands: /ask, /agents, /agent, /new.

use docsgpt_bot::AgentConfig;
use twilight_model::application::command::{Command, CommandType};
use twilight_model::application::interaction::InteractionContextType;
use twilight_model::oauth::ApplicationIntegrationType;
use twilight_util::builder::command::{CommandBuilder, StringBuilder};

/// The commands, with agent choices filled in from the config.
pub fn definitions(agents: &[AgentConfig]) -> Vec<Command> {
    let contexts = [InteractionContextType::Guild, InteractionContextType::BotDm];
    let installs = [ApplicationIntegrationType::GuildInstall];
    let choices: Vec<(String, String)> = agents
        .iter()
        .take(25)
        .map(|a| {
            let label = match &a.description {
                Some(d) => docsgpt_bot::util::truncate_chars(&format!("{} — {d}", a.name), 100),
                None => a.name.clone(),
            };
            (label, a.name.clone())
        })
        .collect();
    let multi = agents.len() > 1;

    let mut ask = CommandBuilder::new("ask", "Ask a question", CommandType::ChatInput)
        .contexts(contexts)
        .integration_types(installs)
        .option(
            StringBuilder::new("question", "What do you want to know?")
                .required(true)
                .max_length(3000),
        );
    if multi {
        ask = ask
            .option(StringBuilder::new("agent", "Which agent answers (this question only)").choices(choices.clone()));
    }
    let mut list = vec![
        ask.build(),
        CommandBuilder::new("new", "Start a new conversation here", CommandType::ChatInput)
            .contexts(contexts)
            .integration_types(installs)
            .build(),
    ];
    if multi {
        list.push(
            CommandBuilder::new("agents", "List the agents", CommandType::ChatInput)
                .contexts(contexts)
                .integration_types(installs)
                .build(),
        );
        list.push(
            CommandBuilder::new(
                "agent",
                "Choose which agent answers in this channel",
                CommandType::ChatInput,
            )
            .contexts(contexts)
            .integration_types(installs)
            .option(StringBuilder::new("name", "Agent").required(true).choices(choices))
            .build(),
        );
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_commands_only_with_several_agents() {
        let one = definitions(&[AgentConfig::new("docs", "k")]);
        assert_eq!(one.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["ask", "new"]);
        assert_eq!(one[0].options.len(), 1);
        let two = definitions(&[
            AgentConfig {
                description: Some("Pricing".into()),
                ..AgentConfig::new("sales", "k")
            },
            AgentConfig::new("docs", "k"),
        ]);
        assert_eq!(
            two.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["ask", "new", "agents", "agent"]
        );
        let choices = two[3].options[0].choices.as_ref().unwrap();
        assert_eq!(choices.len(), 2);
    }
}
