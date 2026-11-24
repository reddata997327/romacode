//! Conversation memory — the Rust mirror of `langchain.memory`
//! (`ConversationBufferMemory`, windowed variant).

use crate::messages::Message;

/// Storage for conversation history used by chains and the agent executor.
pub trait Memory: Send + Sync {
    /// Current history, oldest first.
    fn load(&self) -> Vec<Message>;

    /// Record one user turn and the assistant's final answer.
    fn save_turn(&mut self, user_input: &str, ai_output: &str);

    /// Record arbitrary messages (e.g. full agent trajectory).
    fn push(&mut self, message: Message);

    /// Drop all history.
    fn clear(&mut self);
}

/// Keeps every message forever — `ConversationBufferMemory`.
#[derive(Debug, Default, Clone)]
pub struct ConversationBufferMemory {
    messages: Vec<Message>,
}

impl ConversationBufferMemory {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Memory for ConversationBufferMemory {
    fn load(&self) -> Vec<Message> {
        self.messages.clone()
    }

    fn save_turn(&mut self, user_input: &str, ai_output: &str) {
        self.messages.push(Message::human(user_input));
        self.messages.push(Message::ai(ai_output));
    }

    fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

/// Keeps only the last `k` message pairs — `ConversationBufferWindowMemory`.
#[derive(Debug, Clone)]
pub struct WindowBufferMemory {
    window: usize,
    messages: Vec<Message>,
}

impl WindowBufferMemory {
    /// `window` counts message *pairs*; `2 * window` messages are retained.
    pub fn new(window: usize) -> Self {
        Self { window: window.max(1), messages: Vec::new() }
    }
}

/// An exchange begins at a human (or system) turn; AI/tool messages belong
/// to the exchange that opened before them.
fn is_exchange_start(m: &Message) -> bool {
    matches!(m, Message::Human { .. } | Message::System { .. })
}

/// Index where the trimmed window must start: the last `keep` messages,
/// adjusted to an exchange boundary so the window never OPENS with an
/// orphaned AI/tool message (strict gateways reject mid-pair windows).
fn window_start(msgs: &[Message], keep: usize) -> usize {
    if msgs.len() <= keep {
        return 0;
    }
    let cut = msgs.len() - keep;
    // A clean boundary at or after the cut: drop the partial leading
    // exchange whole.
    let mut start = cut;
    while start < msgs.len() && !is_exchange_start(&msgs[start]) {
        start += 1;
    }
    if start >= msgs.len() {
        // The whole tail is one (in-flight) exchange with no boundary after
        // the cut — keep it intact by walking back to its first message. It
        // will be dropped whole once the next exchange opens.
        start = cut;
        while start > 0 && !is_exchange_start(&msgs[start]) {
            start -= 1;
        }
    }
    start
}

impl Memory for WindowBufferMemory {
    fn load(&self) -> Vec<Message> {
        let start = window_start(&self.messages, self.window * 2);
        self.messages[start..].to_vec()
    }

    fn save_turn(&mut self, user_input: &str, ai_output: &str) {
        self.push(Message::human(user_input));
        self.push(Message::ai(ai_output));
    }

    fn push(&mut self, message: Message) {
        self.messages.push(message);
        let keep = self.window * 2;
        if self.messages.len() > keep {
            let start = window_start(&self.messages, keep);
            self.messages.drain(..start);
        }
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_keeps_everything() {
        let mut m = ConversationBufferMemory::new();
        m.save_turn("hi", "hello");
        m.save_turn("who", "me");
        assert_eq!(m.load().len(), 4);
        m.clear();
        assert!(m.load().is_empty());
    }

    #[test]
    fn window_keeps_last_pairs() {
        let mut m = WindowBufferMemory::new(2);
        for i in 0..5 {
            m.save_turn(&format!("q{i}"), &format!("a{i}"));
        }
        let msgs = m.load();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].text(), "q3");
        assert_eq!(msgs[3].text(), "a4");
    }

    #[test]
    fn window_trims_on_pair_boundaries() {
        // Regression (M36): trimming by plain message count cut message
        // pairs in half — a window opening with an orphaned AI/tool message
        // is rejected by strict gateways. Trim must drop whole exchanges.
        let mut m = WindowBufferMemory::new(1); // keep = 2 messages
        m.push(Message::human("q1"));
        m.push(Message::ai("thinking"));
        m.push(Message::tool("call_1", "search", "result"));
        m.push(Message::ai("a1")); // first exchange complete
        m.push(Message::human("q2"));
        let msgs = m.load();
        // The window must OPEN at a human turn, never with the orphaned
        // AI/tool tail of the first exchange.
        assert!(!msgs.is_empty());
        assert!(matches!(msgs[0], Message::Human { .. }), "window opens mid-pair: {msgs:?}");
        assert_eq!(msgs[0].text(), "q2");

        // Same behavior via `load()` on a buffer trimmed only by `load`.
        let mut m2 = WindowBufferMemory::new(1);
        m2.push(Message::ai("orphan head"));
        m2.push(Message::human("q"));
        m2.push(Message::ai("a"));
        let msgs = m2.load();
        assert!(matches!(msgs[0], Message::Human { .. }), "window opens mid-pair: {msgs:?}");
    }

    #[test]
    fn window_keeps_in_flight_exchange_intact() {
        // When the entire tail is one incomplete exchange, it is kept whole
        // (from its human turn) rather than opening the window mid-pair.
        let mut m = WindowBufferMemory::new(1);
        m.push(Message::human("q1"));
        m.push(Message::ai("mid"));
        m.push(Message::tool("call_1", "calc", "4"));
        m.push(Message::ai("still working"));
        let msgs = m.load();
        assert!(matches!(msgs[0], Message::Human { .. }), "window opens mid-pair: {msgs:?}");
    }
}
