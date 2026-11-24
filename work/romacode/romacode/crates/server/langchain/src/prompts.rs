//! Prompt templates — the Rust mirror of `langchain_core.prompts`
//! (`PromptTemplate`, `ChatPromptTemplate`, `MessagesPlaceholder`).
//!
//! Syntax is the Python f-string style used by LangChain: `{variable}`, with
//! `{{` and `}}` escaping literal braces.

use async_trait::async_trait;
use std::collections::HashMap;

use crate::errors::{LangchainError, Result};
use crate::messages::Message;
use crate::runnables::Runnable;

/// One slot of a [`ChatPromptTemplate`].
#[derive(Debug, Clone)]
pub enum ChatSlot {
    /// A templated system message.
    System(String),
    /// A templated human message.
    Human(String),
    /// A templated AI message (rarely used, but valid).
    AI(String),
    /// `MessagesPlaceholder(variable_name)` — splices message history.
    Placeholder(String),
}

/// A chat prompt assembled from ordered slots — `ChatPromptTemplate.from_messages`.
#[derive(Debug, Clone, Default)]
pub struct ChatPromptTemplate {
    slots: Vec<ChatSlot>,
}

/// Variables accepted by prompt formatting: plain text or message lists.
#[derive(Debug, Clone)]
pub enum PromptValue {
    Text(String),
    Messages(Vec<Message>),
}

impl From<String> for PromptValue {
    fn from(s: String) -> Self {
        PromptValue::Text(s)
    }
}

impl From<&str> for PromptValue {
    fn from(s: &str) -> Self {
        PromptValue::Text(s.to_string())
    }
}

impl From<Vec<Message>> for PromptValue {
    fn from(m: Vec<Message>) -> Self {
        PromptValue::Messages(m)
    }
}

/// Substitute `{var}` placeholders in `template` using `vars`.
/// `{{` and `}}` render as literal `{` and `}`.
///
/// ```ignore
/// format("Hello {name} {{literal}}", &[("name", "Rust")]) == "Hello Rust {literal}"
/// ```
pub fn format_template(template: &str, vars: &HashMap<String, PromptValue>) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                match chars.peek() {
                    Some('{') => {
                        chars.next();
                        out.push('{');
                    }
                    Some('}') => {
                        chars.next();
                        out.push('}');
                    }
                    Some(_) => {
                        let mut name = String::new();
                        let mut closed = false;
                        for c2 in chars.by_ref() {
                            if c2 == '}' {
                                closed = true;
                                break;
                            }
                            name.push(c2);
                        }
                        if !closed {
                            return Err(LangchainError::InvalidTemplate(
                                "unclosed '{' — expected '}'".into(),
                            ));
                        }
                        let name = name.trim();
                        if name.is_empty() {
                            return Err(LangchainError::InvalidTemplate("empty variable name {}".into()));
                        }
                        match vars.get(name) {
                            Some(PromptValue::Text(s)) => out.push_str(s),
                            Some(PromptValue::Messages(msgs)) => {
                                // Flatten message texts into the slot, newline-separated.
                                let joined: Vec<&str> = msgs.iter().map(|m| m.text()).collect();
                                out.push_str(&joined.join("\n"));
                            }
                            None => {
                                return Err(LangchainError::MissingVariable(name.to_string()));
                            }
                        }
                    }
                    None => {
                        return Err(LangchainError::InvalidTemplate(
                            "unterminated '{' at end of template".into(),
                        ))
                    }
                }
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    out.push('}');
                } else {
                    return Err(LangchainError::InvalidTemplate(
                        "stray '}' without matching '{{'".into(),
                    ));
                }
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

impl ChatPromptTemplate {
    /// Build from ordered slots, mirroring `ChatPromptTemplate.from_messages([...])`.
    pub fn from_slots(slots: Vec<ChatSlot>) -> Self {
        Self { slots }
    }

    /// Common shape: one system message + one templated human message.
    pub fn from_system_and_human(system: impl Into<String>, human: impl Into<String>) -> Self {
        Self::from_slots(vec![ChatSlot::System(system.into()), ChatSlot::Human(human.into())])
    }

    /// Convenience builder: system message.
    pub fn with_system(mut self, template: impl Into<String>) -> Self {
        self.slots.push(ChatSlot::System(template.into()));
        self
    }

    /// Convenience builder: templated human message.
    pub fn with_human(mut self, template: impl Into<String>) -> Self {
        self.slots.push(ChatSlot::Human(template.into()));
        self
    }

    /// Convenience builder: message-history placeholder (`MessagesPlaceholder`).
    pub fn with_placeholder(mut self, variable: impl Into<String>) -> Self {
        self.slots.push(ChatSlot::Placeholder(variable.into()));
        self
    }

    /// Format every slot into concrete [`Message`]s.
    pub fn format(&self, vars: &HashMap<String, PromptValue>) -> Result<Vec<Message>> {
        let mut msgs = Vec::new();
        for slot in &self.slots {
            match slot {
                ChatSlot::System(t) => msgs.push(Message::system(format_template(t, vars)?)),
                ChatSlot::Human(t) => msgs.push(Message::human(format_template(t, vars)?)),
                ChatSlot::AI(t) => msgs.push(Message::ai(format_template(t, vars)?)),
                ChatSlot::Placeholder(name) => match vars.get(name) {
                    Some(PromptValue::Messages(history)) => msgs.extend(history.iter().cloned()),
                    Some(PromptValue::Text(t)) => msgs.push(Message::human(t.clone())),
                    None => return Err(LangchainError::MissingVariable(name.clone())),
                },
            }
        }
        Ok(msgs)
    }

    /// Names every `{var}` the slots require (without braces).
    pub fn variables(&self) -> Vec<String> {
        let mut names = Vec::new();
        for slot in &self.slots {
            let t = match slot {
                ChatSlot::System(t) | ChatSlot::Human(t) | ChatSlot::AI(t) => t,
                ChatSlot::Placeholder(n) => {
                    names.push(n.clone());
                    continue;
                }
            };
            names.extend(extract_variables(t));
        }
        names
    }
}

/// Extract variable names mentioned in a template.
pub fn extract_variables(template: &str) -> Vec<String> {
    let mut vars = Vec::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                continue;
            }
            let mut name = String::new();
            for c2 in chars.by_ref() {
                if c2 == '}' {
                    break;
                }
                name.push(c2);
            }
            let name = name.trim().to_string();
            if !name.is_empty() {
                vars.push(name);
            }
        } else if c == '}' && chars.peek() == Some(&'}') {
            chars.next();
        }
    }
    vars
}

#[async_trait]
impl Runnable<HashMap<String, PromptValue>, Vec<Message>> for ChatPromptTemplate {
    async fn invoke(&self, input: HashMap<String, PromptValue>) -> Result<Vec<Message>> {
        self.format(&input)
    }
}

/// A single templated string prompt — `PromptTemplate.from_template("{x}")`.
/// Implements `Runnable`, producing a formatted [`Message::Human`].
#[derive(Debug, Clone)]
pub struct PromptTemplate {
    template: String,
}

impl PromptTemplate {
    /// Create from a `{var}` style template.
    pub fn new(template: impl Into<String>) -> Self {
        Self { template: template.into() }
    }

    /// Format to a plain string.
    pub fn format(&self, vars: &HashMap<String, PromptValue>) -> Result<String> {
        format_template(&self.template, vars)
    }

    /// Variable names required by this template.
    pub fn variables(&self) -> Vec<String> {
        extract_variables(&self.template)
    }
}

#[async_trait]
impl Runnable<HashMap<String, PromptValue>, Message> for PromptTemplate {
    async fn invoke(&self, input: HashMap<String, PromptValue>) -> Result<Message> {
        Ok(Message::human(self.format(&input)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, PromptValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), PromptValue::Text(v.to_string())))
            .collect()
    }

    #[test]
    fn basic_substitution() {
        let t = PromptTemplate::new("Tell me a joke about {topic} for {audience}.");
        assert_eq!(
            t.format(&vars(&[("topic", "Rust"), ("audience", "developers")])).unwrap(),
            "Tell me a joke about Rust for developers."
        );
        assert_eq!(t.variables(), vec!["topic", "audience"]);
    }

    #[test]
    fn brace_escaping() {
        assert_eq!(format_template("{{literal}} {x}", &vars(&[("x", "1")])).unwrap(), "{literal} 1");
    }

    #[test]
    fn missing_variable_errors() {
        let e = format_template("{nope}", &vars(&[])).unwrap_err();
        assert!(matches!(e, LangchainError::MissingVariable(v) if v == "nope"));
    }

    #[test]
    fn unbalanced_templates_error() {
        assert!(format_template("{unclosed", &vars(&[])).is_err());
        assert!(format_template("stray } brace", &vars(&[])).is_err());
    }

    #[test]
    fn chat_prompt_with_placeholder() {
        let prompt = ChatPromptTemplate::from_system_and_human(
            "You are {role}.",
            "Question: {question}",
        )
        .with_placeholder("history");

        let mut v = vars(&[("role", "a pirate"), ("question", "hello?")]);
        let history = vec![Message::human("earlier"), Message::ai("yes")];
        v.insert("history".into(), PromptValue::Messages(history));

        let msgs = prompt.format(&v).unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].text(), "You are a pirate.");
        assert_eq!(msgs[1].text(), "Question: hello?");
        assert_eq!(msgs[2].text(), "earlier");
        assert_eq!(msgs[3].text(), "yes");
    }

    #[tokio::test]
    async fn chat_prompt_is_runnable() {
        let prompt = ChatPromptTemplate::from_system_and_human("sys {a}", "user {b}");
        let m = prompt
            .invoke(vars(&[("a", "A"), ("b", "B")]))
            .await
            .unwrap();
        assert_eq!(m.len(), 2);
    }
}
