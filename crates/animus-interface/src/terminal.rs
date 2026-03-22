use animus_core::error::{AnimusError, Result};
use std::io::{self, BufRead, Write};

/// Terminal-based interface for human interaction.
pub struct TerminalInterface {
    prompt: String,
}

impl TerminalInterface {
    pub fn new(prompt: String) -> Self {
        Self { prompt }
    }

    /// Display a message to the user.
    pub fn display(&self, message: &str) {
        println!("{message}");
    }

    /// Display a system status message.
    pub fn display_status(&self, message: &str) {
        println!("[animus] {message}");
    }

    /// Read a line of input from the user. Returns None on EOF.
    pub fn read_input(&self) -> Result<Option<String>> {
        print!("{}", self.prompt);
        io::stdout()
            .flush()
            .map_err(|e| AnimusError::Interface(format!("stdout flush: {e}")))?;

        let stdin = io::stdin();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => Ok(None), // EOF
            Ok(_) => Ok(Some(line.trim().to_string())),
            Err(e) => Err(AnimusError::Interface(format!("read error: {e}"))),
        }
    }

    /// Display the AILF's response with formatting.
    pub fn display_response(&self, response: &str) {
        println!("\n{response}\n");
    }

    /// Display startup banner.
    pub fn display_banner(&self, instance_id: &str, model: &str, segment_count: usize) {
        println!();
        println!("  ╔══════════════════════════════════════╗");
        println!("  ║           A N I M U S                ║");
        println!("  ║     AI-Native Operating System       ║");
        println!("  ╚══════════════════════════════════════╝");
        println!();
        println!("  Instance:  {instance_id}");
        println!("  Model:     {model}");
        println!("  Segments:  {segment_count}");
        println!();
        println!("  Type /help for commands, /quit to exit.");
        println!();
    }
}
