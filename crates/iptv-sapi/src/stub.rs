//! Non-Windows: no application voice (Python's backend is
//! Windows-only too). Every operation is an unavailable no-op.

pub struct Voice;

impl Voice {
    pub fn probe() -> bool {
        false
    }

    pub fn new() -> Self {
        Self
    }

    pub fn available(&self) -> bool {
        false
    }

    pub fn speak_text(&mut self, _text: &str) -> Result<(), ()> {
        Err(())
    }

    pub fn purge(&mut self) {}

    pub fn set_volume(&mut self, _level: i32) -> bool {
        false
    }

    pub fn set_rate(&mut self, _rate: i32) -> bool {
        false
    }

    pub fn outputs(&mut self) -> Vec<(String, String)> {
        Vec::new()
    }

    pub fn apply_output(&mut self, _target: Option<&str>) -> Result<(), ()> {
        Err(())
    }

    pub fn release(&mut self) {}
}
