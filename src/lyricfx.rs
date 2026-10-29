//! Text effects inside lyrics. A line (or part of one) is wrapped as `{rainbow,wave|words}`;
//! the lyrics editor writes these and the Now playing panel draws them.

pub const ITALIC: u8 = 1;
pub const LOUD: u8 = 2;
pub const RAINBOW: u8 = 4;
pub const WAVE: u8 = 8;
pub const SHAKE: u8 = 16;
/// Effects that move, so the panel has to keep repainting while they are on screen.
pub const MOVING: u8 = RAINBOW | WAVE | SHAKE;

pub const NAMES: [(&str, u8); 5] = [
    ("italic", ITALIC),
    ("loud", LOUD),
    ("rainbow", RAINBOW),
    ("wave", WAVE),
    ("shake", SHAKE),
];

fn flag(name: &str) -> Option<u8> {
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

/// A line with its markup removed: the characters and the effects each one carries, plus where
/// each character started in the original text (for mapping an editor selection).
#[derive(Debug, PartialEq, Default)]
pub struct Parsed {
    pub chars: Vec<char>,
    pub fx: Vec<u8>,
    pub raw: Vec<usize>,
}

impl Parsed {
    pub fn plain(&self) -> String {
        self.chars.iter().collect()
    }
}

/// `{name,name|text}` at char `i` of `chars`: the effects and where the text ends.
fn tag(chars: &[char], i: usize) -> Option<(u8, usize, usize)> {
    let bar = chars[i + 1..].iter().position(|c| matches!(c, '|' | '{' | '}'))? + i + 1;
    if chars[bar] != '|' {
        return None;
    }
    let names: String = chars[i + 1..bar].iter().collect();
    let fx = names
        .split(',')
        .try_fold(0, |all, name| Some(all | flag(name.trim())?))?;
    let close = chars[bar + 1..].iter().position(|c| matches!(c, '{' | '}'))? + bar + 1;
    (chars[close] == '}').then_some((fx, bar + 1, close))
}

pub fn parse(text: &str) -> Parsed {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Parsed::default();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{'
            && let Some((fx, from, to)) = tag(&chars, i)
        {
            for at in from..to {
                out.chars.push(chars[at]);
                out.fx.push(fx);
                out.raw.push(at);
            }
            i = to + 1;
            continue;
        }
        out.chars.push(chars[i]);
        out.fx.push(0);
        out.raw.push(i);
        i += 1;
    }
    out
}

/// Where a line's words start after its `[mm:ss.xx]` stamps, as a char index.
fn body_start(line: &[char]) -> usize {
    let mut at = 0;
    while line.get(at) == Some(&'[') {
        match line[at..].iter().position(|c| *c == ']') {
            Some(close) if line[at + 1..at + close].iter().any(|c| *c == ':') => {
                at += close + 1;
            }
            _ => break,
        }
    }
    at
}

/// Writes `parsed` back as text: runs with an effect go inside `{..|..}`. Braces and bars in the
/// words themselves would end a tag early, so those characters are left unformatted.
fn write(parsed: &Parsed) -> String {
    let mut out = String::new();
    let mut at = 0;
    while at < parsed.chars.len() {
        let plain = |c: char| !matches!(c, '{' | '}' | '|');
        let fx = if plain(parsed.chars[at]) { parsed.fx[at] } else { 0 };
        let mut end = at + 1;
        while end < parsed.chars.len()
            && (if plain(parsed.chars[end]) { parsed.fx[end] } else { 0 }) == fx
        {
            end += 1;
        }
        let run: String = parsed.chars[at..end].iter().collect();
        if fx == 0 {
            out.push_str(&run);
        } else {
            let names: Vec<&str> = NAMES
                .iter()
                .filter(|(_, f)| fx & f != 0)
                .map(|(n, _)| *n)
                .collect();
            out.push_str(&format!("{{{}|{run}}}", names.join(",")));
        }
        at = end;
    }
    out
}

/// Turns `effect` on for the chars `from..to` of `text` (as the editor counts them), or off if
/// they all have it already. With `effect` of `None` every effect is removed. An empty selection
/// means the whole line under `from`. Returns the new text.
pub fn apply(text: &str, from: usize, to: usize, effect: Option<u8>) -> String {
    let (from, to) = (from.min(to), from.max(to));
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        let len = line.chars().count();
        let end = start + len;
        let chars: Vec<char> = line.chars().collect();
        let body = body_start(&chars);
        let whole_line = from == to && (start..=end).contains(&from);
        let touched = whole_line || (from < end && to > start && from != to);
        if !touched {
            out.push(line.to_string());
        } else {
            let head: String = chars[..body].iter().collect();
            let mut parsed = parse(&chars[body..].iter().collect::<String>());
            let selected: Vec<usize> = (0..parsed.chars.len())
                .filter(|i| {
                    whole_line || (from..to).contains(&(start + body + parsed.raw[*i]))
                })
                .collect();
            match effect {
                None => selected.iter().for_each(|i| parsed.fx[*i] = 0),
                Some(effect) => {
                    let all = selected.iter().all(|i| parsed.fx[*i] & effect != 0);
                    for i in &selected {
                        if all {
                            parsed.fx[*i] &= !effect;
                        } else {
                            parsed.fx[*i] |= effect;
                        }
                    }
                }
            }
            out.push(format!("{head}{}", write(&parsed)));
        }
        start = end + 1;
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_read_and_written_back() {
        let parsed = parse("a {rainbow,wave|big} {nope|x} day");
        assert_eq!(parsed.plain(), "a big {nope|x} day");
        assert_eq!(parsed.fx[2], RAINBOW | WAVE);
        assert_eq!(parsed.fx[0], 0);
        assert_eq!(write(&parsed), "a {rainbow,wave|big} {nope|x} day");
    }

    #[test]
    fn applying_toggles_on_a_selection_or_the_whole_line() {
        let text = "[00:01.00]one two three\nsecond";
        // "two" is chars 14..17
        let marked = apply(text, 14, 17, Some(ITALIC));
        assert_eq!(marked, "[00:01.00]one {italic|two} three\nsecond");
        // the selection is in raw text, so the tag sits inside it: still the same word
        assert_eq!(apply(&marked, 14, 25, Some(ITALIC)), text);
        // no selection: the whole line, stamp kept outside
        assert_eq!(
            apply(text, 3, 3, Some(WAVE)),
            "[00:01.00]{wave|one two three}\nsecond"
        );
        assert_eq!(apply(&apply(text, 3, 3, Some(WAVE)), 3, 3, None), text);
        // effects stack
        assert_eq!(
            apply(&marked, 14, 25, Some(SHAKE)),
            "[00:01.00]one {italic,shake|two} three\nsecond"
        );
    }
}
