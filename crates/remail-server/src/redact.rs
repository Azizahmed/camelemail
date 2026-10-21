//! A wrapper that keeps secrets out of logs: `Debug` and `Display` print a
//! placeholder, so a token can't end up in a `tracing` field by accident.

use std::fmt;

use zeroize::Zeroize;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Redacted<T: Zeroize>(T);

impl<T: Zeroize> Redacted<T> {
    pub fn new(value: T) -> Self {
        Redacted(value)
    }

    /// The secret itself. Keep the borrow short and never log it.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T: Zeroize> Drop for Redacted<T> {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl<T: Zeroize> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<T: Zeroize> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl From<String> for Redacted<String> {
    fn from(s: String) -> Self {
        Redacted(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_prints() {
        let r = Redacted::new("secret-token".to_string());
        assert_eq!(format!("{r:?} {r}"), "[redacted] [redacted]");
        assert_eq!(r.expose(), "secret-token");
    }
}
