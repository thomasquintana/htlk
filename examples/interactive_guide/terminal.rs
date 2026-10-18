//! Line-oriented terminal I/O, also usable with scripted input in tests.
use crate::Result;
use std::{
    fmt,
    io::{BufRead, Write},
    sync::{Arc, Mutex},
};

#[derive(Debug, PartialEq, Eq)]
pub enum Navigation {
    Menu,
    Quit,
}
impl fmt::Display for Navigation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Navigation {}

pub struct Terminal<R, W> {
    reader: Arc<Mutex<R>>,
    pub output: W,
}
impl<R: BufRead + Send + 'static, W: Write> Terminal<R, W> {
    pub fn new(reader: R, output: W) -> Self {
        Self {
            reader: Arc::new(Mutex::new(reader)),
            output,
        }
    }

    pub fn say(&mut self, text: impl fmt::Display) -> Result<()> {
        writeln!(self.output, "{text}")?;
        Ok(())
    }

    pub async fn ask(&mut self, prompt: &str, default: Option<&str>) -> Result<String> {
        write!(self.output, "{prompt}")?;
        if let Some(value) = default {
            write!(self.output, " [{value}]")?;
        }
        write!(self.output, ": ")?;
        self.output.flush()?;
        let reader = self.reader.clone();
        // Blocking stdin must not stop actor heartbeats, deadlines, or wait handling.
        let line = tokio::task::spawn_blocking(move || {
            let mut line = String::new();
            let count = reader
                .lock()
                .map_err(|_| std::io::Error::other("terminal input lock poisoned"))?
                .read_line(&mut line)?;
            Ok::<_, std::io::Error>((count, line))
        })
        .await??;
        if line.0 == 0 {
            return Err(Navigation::Quit.into());
        }
        let answer = line.1.trim();
        match answer {
            "q" | "quit" => Err(Navigation::Quit.into()),
            "m" | "menu" => Err(Navigation::Menu.into()),
            "" => Ok(default.unwrap_or_default().into()),
            _ => Ok(answer.into()),
        }
    }

    pub async fn integer(&mut self, prompt: &str, default: &str) -> Result<i64> {
        loop {
            let answer = self.ask(prompt, Some(default)).await?;
            match answer.parse::<i64>() {
                Ok(value) if (-1_000_000..=1_000_000).contains(&value) => return Ok(value),
                _ => self.say("Enter a whole number from -1000000 to 1000000. Example: 7")?,
            }
        }
    }

    pub async fn choice(
        &mut self,
        prompt: &str,
        choices: &[&str],
        default: &str,
    ) -> Result<String> {
        loop {
            let answer = self.ask(prompt, Some(default)).await?.to_lowercase();
            if choices.contains(&answer.as_str()) {
                return Ok(answer);
            }
            self.say(format!(
                "Choose {}. Type m for the menu or q to quit.",
                choices.join(" / ")
            ))?;
        }
    }
}
