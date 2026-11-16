//! Codex's `exec` tool: the model writes a JavaScript snippet that calls the real tools on a
//! global `tools` object, e.g.
//!
//! ```text
//! text(await tools.exec_command({cmd:"echo hi > hello.txt",max_output_tokens:1000}));
//! text(await tools.apply_patch("*** Begin Patch\n*** Add File: a.txt\n+Green\n*** End Patch"));
//! ```
//!
//! Codex 0.169's hooks already report the inner call (`Bash {command}`, `apply_patch {command}`),
//! but a hook or a Codex version that passes the snippet through as tool `exec` would otherwise
//! show raw JavaScript. This reads the snippet without running it: every `tools.<name>(<arg>)`
//! call outside strings and comments, with a string-literal argument decoded (escapes included)
//! and an object-literal argument turned into JSON (string, number, boolean and null values;
//! anything computed is left out). Template literals keep `${...}` placeholders as written.

use serde_json::{Map, Value, json};

/// The tool calls in an `exec` snippet, in order, as `(tool name, argument)`. A string argument
/// is returned as a JSON string, an object literal as a JSON object, anything else as null.
pub fn tool_calls(src: &str) -> Vec<(String, Value)> {
    let chars: Vec<char> = src.chars().collect();
    let mut p = Parser { s: &chars, i: 0 };
    let mut calls = Vec::new();
    while p.i < chars.len() {
        let c = chars[p.i];
        match c {
            '"' | '\'' | '`' => {
                p.string();
            }
            '/' if p.peek(1) == Some('/') => p.line_comment(),
            '/' if p.peek(1) == Some('*') => p.block_comment(),
            _ if p.at_word("tools") => {
                let start = p.i;
                p.i += "tools".len();
                if let Some(call) = p.call() {
                    calls.push(call);
                } else {
                    p.i = start + 1;
                }
            }
            _ => p.i += 1,
        }
    }
    calls
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.s.get(self.i + ahead).copied()
    }

    fn is_ident(c: char) -> bool {
        c.is_alphanumeric() || c == '_' || c == '$'
    }

    /// `word` starts here as a whole identifier (not `mytools` or `tools2`)
    fn at_word(&self, word: &str) -> bool {
        let w: Vec<char> = word.chars().collect();
        if self.s.len() < self.i + w.len() || self.s[self.i..self.i + w.len()] != w[..] {
            return false;
        }
        let before_ok = self.i == 0 || {
            let b = self.s[self.i - 1];
            !Self::is_ident(b) && b != '.'
        };
        let after_ok = self
            .s
            .get(self.i + w.len())
            .is_none_or(|c| !Self::is_ident(*c));
        before_ok && after_ok
    }

    fn skip_ws(&mut self) {
        loop {
            match self.peek(0) {
                Some(c) if c.is_whitespace() => self.i += 1,
                Some('/') if self.peek(1) == Some('/') => self.line_comment(),
                Some('/') if self.peek(1) == Some('*') => self.block_comment(),
                _ => break,
            }
        }
    }

    fn line_comment(&mut self) {
        while let Some(c) = self.peek(0) {
            self.i += 1;
            if c == '\n' {
                break;
            }
        }
    }

    fn block_comment(&mut self) {
        self.i += 2;
        while self.i < self.s.len() {
            if self.peek(0) == Some('*') && self.peek(1) == Some('/') {
                self.i += 2;
                return;
            }
            self.i += 1;
        }
    }

    fn ident(&mut self) -> Option<String> {
        let start = self.i;
        while self.peek(0).is_some_and(Self::is_ident) {
            self.i += 1;
        }
        (self.i > start).then(|| self.s[start..self.i].iter().collect())
    }

    /// After `tools`: `.name(arg)` or `["name"](arg)`. Leaves `i` after the call's `)` (or
    /// wherever the argument ended when it could not be read).
    fn call(&mut self) -> Option<(String, Value)> {
        self.skip_ws();
        let name = match self.peek(0)? {
            '.' => {
                self.i += 1;
                self.skip_ws();
                self.ident()?
            }
            '[' => {
                self.i += 1;
                self.skip_ws();
                let name = self.string()?;
                self.skip_ws();
                if self.peek(0)? != ']' {
                    return None;
                }
                self.i += 1;
                name
            }
            _ => return None,
        };
        self.skip_ws();
        if self.peek(0)? != '(' {
            return None;
        }
        self.i += 1;
        self.skip_ws();
        let arg = if self.peek(0) == Some(')') {
            Value::Null
        } else {
            let value = self.value();
            self.skip_ws();
            // `"a" + x` or `f(...)`: what we read is only part of it
            match value {
                Some(v) if matches!(self.peek(0), Some(')' | ',')) => v,
                _ => Value::Null,
            }
        };
        Some((name, arg))
    }

    /// A value we can read without running anything: a string (or strings joined with `+`), an
    /// object literal, a number, true, false or null. None skips the expression.
    fn value(&mut self) -> Option<Value> {
        self.skip_ws();
        match self.peek(0)? {
            '"' | '\'' | '`' => {
                let mut s = self.string()?;
                loop {
                    let save = self.i;
                    self.skip_ws();
                    if self.peek(0) == Some('+') {
                        self.i += 1;
                        self.skip_ws();
                        if matches!(self.peek(0), Some('"' | '\'' | '`')) {
                            s.push_str(&self.string()?);
                            continue;
                        }
                    }
                    self.i = save;
                    break;
                }
                Some(Value::String(s))
            }
            '{' => self.object(),
            c if c.is_ascii_digit() || c == '-' => {
                let start = self.i;
                self.i += 1;
                while self
                    .peek(0)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
                {
                    self.i += 1;
                }
                let text: String = self.s[start..self.i]
                    .iter()
                    .filter(|c| **c != '_')
                    .collect();
                text.parse::<i64>()
                    .map(Value::from)
                    .ok()
                    .or_else(|| text.parse::<f64>().ok().map(|f| json!(f)))
            }
            _ => {
                let start = self.i;
                match self.ident().as_deref() {
                    Some("true") => Some(Value::Bool(true)),
                    Some("false") => Some(Value::Bool(false)),
                    Some("null") => Some(Value::Null),
                    _ => {
                        self.i = start;
                        None
                    }
                }
            }
        }
    }

    /// `{ key: value, "key": value, ... }`. Values we cannot read are skipped and left out.
    fn object(&mut self) -> Option<Value> {
        self.i += 1; // {
        let mut map = Map::new();
        loop {
            self.skip_ws();
            match self.peek(0)? {
                '}' => {
                    self.i += 1;
                    return Some(Value::Object(map));
                }
                ',' => {
                    self.i += 1;
                    continue;
                }
                '"' | '\'' => {
                    let key = self.string()?;
                    self.entry(&mut map, key)?;
                }
                _ => {
                    let key = self.ident()?;
                    self.entry(&mut map, key)?;
                }
            }
        }
    }

    fn entry(&mut self, map: &mut Map<String, Value>, key: String) -> Option<()> {
        self.skip_ws();
        if self.peek(0)? != ':' {
            // Shorthand `{ cmd }`: a variable we cannot see
            return Some(());
        }
        self.i += 1;
        let save = self.i;
        match self.value() {
            Some(v) => {
                // Anything after a readable start (`"a" + x`) makes the value a guess: skip it
                self.skip_ws();
                if matches!(self.peek(0), Some(',' | '}')) {
                    map.insert(key, v);
                } else {
                    self.skip_expression();
                }
            }
            None => {
                self.i = save;
                self.skip_expression();
            }
        }
        Some(())
    }

    /// Skips to the `,` or `}` that ends this property value, stepping over nested brackets,
    /// strings and comments.
    fn skip_expression(&mut self) {
        let mut depth = 0i32;
        while let Some(c) = self.peek(0) {
            match c {
                '"' | '\'' | '`' => {
                    self.string();
                    continue;
                }
                '/' if self.peek(1) == Some('/') => {
                    self.line_comment();
                    continue;
                }
                '/' if self.peek(1) == Some('*') => {
                    self.block_comment();
                    continue;
                }
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => return,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => return,
                _ => {}
            }
            self.i += 1;
        }
    }

    /// A string or template literal at `i`, decoded. Leaves `i` after the closing quote.
    /// A template's `${...}` placeholders are kept as written.
    fn string(&mut self) -> Option<String> {
        let quote = self.peek(0)?;
        self.i += 1;
        let mut out = String::new();
        while let Some(c) = self.peek(0) {
            self.i += 1;
            match c {
                _ if c == quote => return Some(out),
                '\\' => self.escape(&mut out, quote == '`'),
                '$' if quote == '`' && self.peek(0) == Some('{') => {
                    // Copy the placeholder through, matching braces
                    out.push('$');
                    let mut depth = 0;
                    while let Some(c) = self.peek(0) {
                        out.push(c);
                        self.i += 1;
                        match c {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                '\n' if quote != '`' => return Some(out), // unterminated: keep what we have
                _ => out.push(c),
            }
        }
        Some(out)
    }

    fn hex(&mut self, digits: usize) -> Option<u32> {
        let text: String = self.s.get(self.i..self.i + digits)?.iter().collect();
        let n = u32::from_str_radix(&text, 16).ok()?;
        self.i += digits;
        Some(n)
    }

    fn escape(&mut self, out: &mut String, template: bool) {
        let Some(c) = self.peek(0) else { return };
        self.i += 1;
        match c {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' if !self.peek(0).is_some_and(|d| d.is_ascii_digit()) => out.push('\0'),
            'x' => match self.hex(2).and_then(char::from_u32) {
                Some(ch) => out.push(ch),
                None => out.push('x'),
            },
            'u' => {
                let unit = if self.peek(0) == Some('{') {
                    let close = self.s[self.i..].iter().position(|c| *c == '}');
                    close.and_then(|n| {
                        let text: String = self.s[self.i + 1..self.i + n].iter().collect();
                        let v = u32::from_str_radix(&text, 16).ok()?;
                        self.i += n + 1;
                        Some(v)
                    })
                } else {
                    self.hex(4)
                };
                match unit {
                    // A surrogate pair: 😀
                    Some(hi @ 0xD800..=0xDBFF)
                        if self.peek(0) == Some('\\') && self.peek(1) == Some('u') =>
                    {
                        let save = self.i;
                        self.i += 2;
                        match self.hex(4) {
                            Some(lo @ 0xDC00..=0xDFFF) => {
                                let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                out.extend(char::from_u32(cp));
                            }
                            _ => {
                                self.i = save;
                                out.push('\u{FFFD}');
                            }
                        }
                    }
                    Some(v) => out.push(char::from_u32(v).unwrap_or('\u{FFFD}')),
                    None => out.push('u'),
                }
            }
            // A backslash before a newline continues the line
            '\n' => {}
            '\r' => {
                if self.peek(0) == Some('\n') {
                    self.i += 1;
                }
            }
            '$' if template => out.push('$'),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exec_command_from_codex_0_169() {
        let src = r#"text(await tools.exec_command({cmd:"echo hi > hello.txt",max_output_tokens:1000}));"#;
        let calls = tool_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "exec_command");
        assert_eq!(calls[0].1["cmd"], "echo hi > hello.txt");
        assert_eq!(calls[0].1["max_output_tokens"], 1000);
    }

    #[test]
    fn decodes_the_apply_patch_string() {
        let src = r#"text(await tools.apply_patch("*** Begin Patch\n*** Add File: /Users/jack/codex-test-proj/colour.txt\n+Green\n*** End Patch"));"#;
        let calls = tool_calls(src);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "apply_patch");
        assert_eq!(
            calls[0].1,
            "*** Begin Patch\n*** Add File: /Users/jack/codex-test-proj/colour.txt\n+Green\n*** End Patch"
        );
    }

    #[test]
    fn decodes_escapes() {
        let src = r#"tools.x("a\"b\\c\'d\x41B\u{1F600}😀\t|")"#;
        assert_eq!(tool_calls(src)[0].1, "a\"b\\c'dAB\u{1F600}\u{1F600}\t|");
        let single = r#"tools.x('it\'s "fine"')"#;
        assert_eq!(tool_calls(single)[0].1, "it's \"fine\"");
    }

    #[test]
    fn reads_template_literals_and_joined_strings() {
        let src = "await tools.apply_patch(`*** Begin Patch\n*** Update File: a.rs\n-x\n+y ${n}\n*** End Patch`)";
        assert_eq!(
            tool_calls(src)[0].1,
            "*** Begin Patch\n*** Update File: a.rs\n-x\n+y ${n}\n*** End Patch"
        );
        let joined = r#"tools.exec_command({ "cmd": "git " + 'status', yield_time_ms: 10_000 })"#;
        let calls = tool_calls(joined);
        assert_eq!(calls[0].1["cmd"], "git status");
        assert_eq!(calls[0].1["yield_time_ms"], 10000);
    }

    #[test]
    fn finds_every_call_and_ignores_strings_and_comments() {
        let src = r#"// @exec: {"yield_time_ms": 10000}
/* tools.fake("no") */
const note = "tools.alsofake('no')";
const a = await tools.exec_command({cmd: "ls", workdir: `/tmp`, extra: {deep: [1, 2]}, after: "kept"});
text(a);
text(await tools["apply_patch"]({input: "*** Begin Patch\n*** Delete File: old.txt\n*** End Patch"}));
mytools.nope("x");"#;
        let calls = tool_calls(src);
        let names: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
        assert_eq!(names, vec!["exec_command", "apply_patch"]);
        assert_eq!(calls[0].1["cmd"], "ls");
        assert_eq!(calls[0].1["workdir"], "/tmp");
        assert_eq!(calls[0].1["after"], "kept");
        assert_eq!(calls[0].1["extra"], json!({})); // arrays are not read
        assert_eq!(
            calls[1].1["input"],
            "*** Begin Patch\n*** Delete File: old.txt\n*** End Patch"
        );
    }

    #[test]
    fn computed_values_are_left_out() {
        let src = r#"const c = "ls"; await tools.exec_command({cmd: c}); await tools.exec_command({cmd: "a" + c}); tools.view_image(path)"#;
        let calls = tool_calls(src);
        assert_eq!(calls.len(), 3);
        assert!(calls[0].1.get("cmd").is_none());
        assert!(calls[1].1.get("cmd").is_none());
        assert!(calls[2].1.is_null());
    }

    #[test]
    fn survives_broken_input() {
        for src in [
            "tools.",
            "tools.x(",
            "tools.x(\"unterminated",
            "tools.x({cmd:",
            "tools",
            "",
        ] {
            let _ = tool_calls(src);
        }
        assert!(tool_calls("not js at all").is_empty());
    }
}
