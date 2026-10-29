//! Text effects and hand-set rhymes inside lyrics. Words are wrapped as `{rainbow,wave|words}`,
//! `{rhyme3|words}` (rhyme colour 3) or `{norhyme|words}` (never coloured); the lyrics editor
//! writes these and the Now playing panel draws them.

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

/// Rhyme value meaning "do not colour this word".
pub const NO_RHYME: u8 = 9;
/// How many hand-set rhyme colours there are (`rhyme1` to `rhyme8`).
pub const RHYME_COLORS: u8 = 8;

/// What a tag name means: effect bits and a rhyme value (0 when it is not a rhyme).
fn meaning(name: &str) -> Option<(u8, u8)> {
    if let Some((_, f)) = NAMES.iter().find(|(n, _)| *n == name) {
        return Some((*f, 0));
    }
    if name == "norhyme" {
        return Some((0, NO_RHYME));
    }
    let n: u8 = name.strip_prefix("rhyme")?.parse().ok()?;
    (1..=RHYME_COLORS).contains(&n).then_some((0, n))
}

fn rhyme_name(rhyme: u8) -> String {
    if rhyme == NO_RHYME {
        "norhyme".into()
    } else {
        format!("rhyme{rhyme}")
    }
}

/// A line with its markup removed: the characters and the effects each one carries, plus where
/// each character started in the original text (for mapping an editor selection).
#[derive(Debug, PartialEq, Default)]
pub struct Parsed {
    pub chars: Vec<char>,
    pub fx: Vec<u8>,
    /// 0 = worked out automatically, 1 to 8 = that colour, `NO_RHYME` = never coloured.
    pub rhyme: Vec<u8>,
    pub raw: Vec<usize>,
}

impl Parsed {
    pub fn plain(&self) -> String {
        self.chars.iter().collect()
    }
}

/// `{name,name|text}` at char `i` of `chars`: effects, rhyme, and where the text starts and ends.
fn tag(chars: &[char], i: usize) -> Option<(u8, u8, usize, usize)> {
    let bar = chars[i + 1..]
        .iter()
        .position(|c| matches!(c, '|' | '{' | '}'))?
        + i
        + 1;
    if chars[bar] != '|' {
        return None;
    }
    let names: String = chars[i + 1..bar].iter().collect();
    let (fx, rhyme) = names.split(',').try_fold((0, 0), |(fx, rhyme), name| {
        let (f, r) = meaning(name.trim())?;
        Some((fx | f, if r != 0 { r } else { rhyme }))
    })?;
    let close = chars[bar + 1..]
        .iter()
        .position(|c| matches!(c, '{' | '}'))?
        + bar
        + 1;
    (chars[close] == '}').then_some((fx, rhyme, bar + 1, close))
}

pub fn parse(text: &str) -> Parsed {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Parsed::default();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{'
            && let Some((fx, rhyme, from, to)) = tag(&chars, i)
        {
            for (at, c) in chars.iter().enumerate().take(to).skip(from) {
                out.chars.push(*c);
                out.fx.push(fx);
                out.rhyme.push(rhyme);
                out.raw.push(at);
            }
            i = to + 1;
            continue;
        }
        out.chars.push(chars[i]);
        out.fx.push(0);
        out.rhyme.push(0);
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
            Some(close) if line[at + 1..at + close].contains(&':') => {
                at += close + 1;
            }
            _ => break,
        }
    }
    at
}

/// Writes `parsed` back as text: runs with an effect or a rhyme go inside `{..|..}`. Braces and
/// bars in the words themselves would end a tag early, so those characters are left plain.
fn write(parsed: &Parsed) -> String {
    let mut out = String::new();
    let mut at = 0;
    let plain = |c: char| !matches!(c, '{' | '}' | '|');
    let key = |i: usize| {
        if plain(parsed.chars[i]) {
            (parsed.fx[i], parsed.rhyme[i])
        } else {
            (0, 0)
        }
    };
    while at < parsed.chars.len() {
        let (fx, rhyme) = key(at);
        let mut end = at + 1;
        while end < parsed.chars.len() && key(end) == (fx, rhyme) {
            end += 1;
        }
        let run: String = parsed.chars[at..end].iter().collect();
        if (fx, rhyme) == (0, 0) {
            out.push_str(&run);
        } else {
            let mut names: Vec<String> = NAMES
                .iter()
                .filter(|(_, f)| fx & f != 0)
                .map(|(n, _)| n.to_string())
                .collect();
            if rhyme != 0 {
                names.push(rhyme_name(rhyme));
            }
            out.push_str(&format!("{{{}|{run}}}", names.join(",")));
        }
        at = end;
    }
    out
}

/// What the editor's buttons do to the selected words.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edit {
    /// Turn an effect on, or off if all the words have it already.
    Toggle(u8),
    /// Remove every effect (rhymes stay).
    ClearEffects,
    /// Colour the words as rhyme `1..=8`, or `NO_RHYME`; again to undo. `0` goes back to automatic.
    Rhyme(u8),
}

/// Applies `edit` to the chars `from..to` of `text` (as the editor counts them). An empty
/// selection means the whole line under `from`. Returns the new text.
pub fn apply(text: &str, from: usize, to: usize, edit: Edit) -> String {
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
            let mut selected: Vec<usize> = (0..parsed.chars.len())
                .filter(|i| whole_line || (from..to).contains(&(start + body + parsed.raw[*i])))
                .collect();
            // a rhyme with nothing selected means the line's last word, not the whole line
            if whole_line && matches!(edit, Edit::Rhyme(_)) {
                let word =
                    |i: &usize| parsed.chars[*i].is_alphanumeric() || parsed.chars[*i] == '\'';
                if let Some(last) = selected.iter().rposition(word) {
                    let first = selected[..=last]
                        .iter()
                        .rposition(|i| parsed.chars[*i].is_whitespace())
                        .map_or(0, |at| at + 1);
                    selected = selected[first..=last].to_vec();
                }
            }
            match edit {
                Edit::ClearEffects => selected.iter().for_each(|i| parsed.fx[*i] = 0),
                Edit::Toggle(effect) => {
                    let all = selected.iter().all(|i| parsed.fx[*i] & effect != 0);
                    for i in &selected {
                        if all {
                            parsed.fx[*i] &= !effect;
                        } else {
                            parsed.fx[*i] |= effect;
                        }
                    }
                }
                Edit::Rhyme(value) => {
                    let all = selected.iter().all(|i| parsed.rhyme[*i] == value);
                    for i in &selected {
                        parsed.rhyme[*i] = if all { 0 } else { value };
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
        let rhymed = parse("one {rhyme3|two} {wave,norhyme|three}");
        assert_eq!(rhymed.plain(), "one two three");
        assert_eq!(
            (rhymed.rhyme[4], rhymed.rhyme[8], rhymed.fx[8]),
            (3, NO_RHYME, WAVE)
        );
        assert_eq!(write(&rhymed), "one {rhyme3|two} {wave,norhyme|three}");
        assert_eq!(parse("{rhyme9|x}").plain(), "{rhyme9|x}");
        assert_eq!(write(&parsed), "a {rainbow,wave|big} {nope|x} day");
    }

    #[test]
    fn applying_toggles_on_a_selection_or_the_whole_line() {
        let text = "[00:01.00]one two three\nsecond";
        // "two" is chars 14..17
        let marked = apply(text, 14, 17, Edit::Toggle(ITALIC));
        assert_eq!(marked, "[00:01.00]one {italic|two} three\nsecond");
        // the selection is in raw text, so the tag sits inside it: still the same word
        assert_eq!(apply(&marked, 14, 25, Edit::Toggle(ITALIC)), text);
        // no selection: the whole line, stamp kept outside
        assert_eq!(
            apply(text, 3, 3, Edit::Toggle(WAVE)),
            "[00:01.00]{wave|one two three}\nsecond"
        );
        assert_eq!(
            apply(
                &apply(text, 3, 3, Edit::Toggle(WAVE)),
                3,
                3,
                Edit::ClearEffects
            ),
            text
        );
        // a rhyme colour on the same word as an effect, and again to undo it
        let coloured = apply(text, 14, 17, Edit::Rhyme(2));
        assert_eq!(coloured, "[00:01.00]one {rhyme2|two} three\nsecond");
        assert_eq!(
            apply(&coloured, 14, 25, Edit::Toggle(WAVE)),
            "[00:01.00]one {wave,rhyme2|two} three\nsecond"
        );
        assert_eq!(apply(&coloured, 14, 25, Edit::Rhyme(2)), text);
        // with nothing selected a rhyme goes on the last word only
        assert_eq!(
            apply(text, 3, 3, Edit::Rhyme(1)),
            "[00:01.00]one two {rhyme1|three}\nsecond"
        );
        // effects stack
        assert_eq!(
            apply(&marked, 14, 25, Edit::Toggle(SHAKE)),
            "[00:01.00]one {italic,shake|two} three\nsecond"
        );
    }
}
