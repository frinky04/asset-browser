//! What the search box means: free words, `"exact phrases"`, `-exclusions`, `either|or`, and
//! fields such as `is:normal`, `pack:mega`, `size:>1mb`, `res:>=1024`, `tris:<500`, `dur:<2s`.
//!
//! A field that's still being typed (`size:>`) asks for nothing, so the grid doesn't blank
//! halfway through a word.

use crate::library::{Asset, Kind, Library};
use crate::thumbs::Facts;

/// Words people use for one another, so `gun` finds a pistol. Literal matches still rank first.
const RELATED: &[&[&str]] = &[
    &["gun", "pistol", "rifle", "shotgun", "revolver", "smg", "handgun", "firearm"],
    &["knife", "blade", "dagger", "machete", "cleaver"],
    &["sword", "blade", "katana", "sabre", "saber"],
    &["axe", "hatchet"],
    &["rock", "stone", "boulder", "pebble"],
    &["wood", "timber", "plank", "lumber", "log"],
    &["metal", "steel", "iron", "tin"],
    &["concrete", "cement"],
    &["dirt", "soil", "mud", "ground"],
    &["grass", "foliage", "fern", "bush", "shrub", "plant"],
    &["tree", "pine", "oak", "birch"],
    &["leaf", "leaves", "foliage"],
    &["water", "river", "lake", "ocean", "sea"],
    &["fire", "flame", "burning"],
    &["blood", "gore"],
    &["zombie", "undead", "ghoul", "corpse"],
    &["monster", "creature"],
    &["character", "human", "person", "npc"],
    &["door", "gate", "hatch"],
    &["car", "vehicle", "truck", "van"],
    &["light", "lamp", "lantern", "torch"],
    &["chair", "stool", "seat", "bench"],
    &["couch", "sofa"],
    &["table", "desk"],
    &["bed", "mattress"],
    &["bottle", "flask", "jar"],
    &["box", "crate", "chest"],
    &["barrel", "drum", "keg"],
    &["trash", "garbage", "rubbish", "junk", "debris"],
    &["footstep", "footsteps", "step", "walk"],
    &["explosion", "blast", "boom", "explode"],
    &["scream", "shout", "yell"],
    &["wind", "breeze"],
    &["rain", "storm", "thunder"],
    &["ambience", "ambient", "atmosphere", "drone"],
    &["click", "tap"],
    &["music", "song"],
    &["albedo", "diffuse", "basecolor"],
    &["color", "colour"],
    &["gray", "grey"],
];

/// What `is:` can ask about. The first word of a row is the one suggested.
const FLAGS: &[(&[&str], Flag)] = &[
    (&["model", "models", "mesh", "3d"], Flag::Kind(Kind::Model)),
    (&["texture", "textures", "image", "img", "tex"], Flag::Kind(Kind::Texture)),
    (&["audio", "sound", "sfx"], Flag::Kind(Kind::Audio)),
    (&["normal", "nrm"], Flag::Map("normal")),
    (&["roughness", "rough", "gloss"], Flag::Map("roughness")),
    (&["metallic", "metal"], Flag::Map("metallic")),
    (&["ao", "occlusion"], Flag::Map("ao")),
    (&["height", "displacement", "bump"], Flag::Map("height")),
    (&["emissive", "emission", "glow"], Flag::Map("emissive")),
    (&["specular", "spec"], Flag::Map("specular")),
    (&["opacity", "alpha", "mask"], Flag::Map("opacity")),
    (&["color", "albedo", "diffuse", "basecolor"], Flag::Map("color")),
    (&["data"], Flag::AnyDataMap),
    (&["square"], Flag::Square),
    (&["pot", "poweroftwo"], Flag::PowerOfTwo),
    (&["wide", "landscape"], Flag::Wide),
    (&["tall", "portrait"], Flag::Tall),
    (&["broken", "failed"], Flag::Broken),
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Flag {
    Kind(Kind),
    Map(&'static str),
    AnyDataMap,
    Square,
    PowerOfTwo,
    Wide,
    Tall,
    Broken,
}

impl Flag {
    fn needs_facts(self) -> bool {
        matches!(self, Flag::Square | Flag::PowerOfTwo | Flag::Wide | Flag::Tall | Flag::Broken)
    }
}

/// What an `is:` value asks about, an unfinished one being taken as the first it could become.
fn resolve_flag(value: &str) -> Option<Flag> {
    let exact = FLAGS.iter().find(|(words, _)| words.contains(&value));
    exact.or_else(|| FLAGS.iter().find(|(words, _)| words.iter().any(|w| w.starts_with(value)))).map(|(_, flag)| *flag)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Range {
    min: f64,
    max: f64,
}

impl Range {
    fn holds(self, v: f64) -> bool {
        v >= self.min && v <= self.max
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Text {
    raw: String,
    /// `raw` the way asset names are split into words, so `wood door` finds `wood_door`.
    spaced: String,
    /// In quotes: whole words only, and nothing related.
    exact: bool,
    related: Vec<&'static str>,
}

impl Text {
    fn new(word: &str, exact: bool) -> Text {
        let raw = word.to_lowercase();
        let spaced = crate::library::words_of(word);
        let related = if exact { Vec::new() } else { related_to(&raw) };
        Text { raw, spaced, exact, related }
    }

    /// How well it fits, lower being better, or `None` if it doesn't.
    fn fit(&self, a: &Asset) -> Option<u32> {
        let name = name_part(&a.words);
        if self.exact {
            if word_hit(name, &self.spaced, false) { return Some(0) }
            return word_hit(&a.words, &self.spaced, false).then_some(3);
        }
        let in_words = self.spaced != self.raw && a.words.contains(&self.spaced);
        if a.hay.contains(&self.raw) || in_words {
            if word_hit(name, &self.spaced, false) { return Some(0) }
            if word_hit(name, &self.spaced, true) { return Some(1) }
            if name_part(&a.hay).contains(&self.raw) { return Some(2) }
            return Some(3);
        }
        // Only in the name: a hinge in `Guns/` isn't what someone searching `pistol` means.
        self.related.iter().any(|w| word_hit(name, w, true)).then_some(4)
    }
}

fn related_to(word: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = RELATED.iter().filter(|g| g.contains(&word)).flat_map(|g| g.iter().copied()).filter(|w| *w != word).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// After the last `/`.
fn name_part(path: &str) -> &str {
    path.rfind('/').map_or(path, |i| &path[i + 1..])
}

/// Whether `needle` occurs starting on a word, and ending on one unless `prefix`.
fn word_hit(hay: &str, needle: &str, prefix: bool) -> bool {
    if needle.is_empty() { return false }
    let bytes = hay.as_bytes();
    let edge = |b: u8| b == b' ' || b == b'/';
    hay.match_indices(needle).any(|(at, _)| {
        let end = at + needle.len();
        (at == 0 || edge(bytes[at - 1])) && (prefix || end == bytes.len() || edge(bytes[end]))
    })
}

#[derive(Clone, Debug, PartialEq)]
enum Check {
    Text(Text),
    Name(String),
    Folder(String),
    Pack(String),
    Ext(&'static str),
    Flag(Flag),
    Size(Range),
    Age(Range),
    Res(Range),
    Width(Range),
    Height(Range),
    Tris(Range),
    Seconds(Range),
    Any(Vec<Check>),
}

/// What's needed to test an asset beyond the asset itself.
struct Context<'a> {
    now: u64,
    facts: &'a dyn Fn(u64) -> Option<Facts>,
}

impl Check {
    fn needs_facts(&self) -> bool {
        match self {
            Check::Res(_) | Check::Width(_) | Check::Height(_) | Check::Tris(_) | Check::Seconds(_) => true,
            Check::Flag(f) => f.needs_facts(),
            Check::Any(all) => all.iter().any(Check::needs_facts),
            _ => false,
        }
    }

    /// How well it fits, lower being better, or `None` if it doesn't.
    fn fit(&self, a: &Asset, cx: &Context<'_>) -> Option<u32> {
        let yes = |ok: bool| ok.then_some(0);
        let known = |kind: Kind| if a.kind == kind { (cx.facts)(a.key).filter(|f| !f.failed) } else { None };
        match self {
            Check::Text(t) => t.fit(a),
            Check::Name(v) => yes(name_part(&a.hay).contains(v.as_str()) || name_part(&a.words).contains(v.as_str())),
            Check::Folder(v) => {
                let at = a.hay.rfind('/').unwrap_or(0);
                yes(a.hay[..at].contains(v.as_str()))
            }
            Check::Pack(v) => {
                let pack = a.hay.split_once('/').map_or(crate::library::LOOSE_PACK_LOWER, |(p, _)| p);
                yes(pack.contains(v.as_str()))
            }
            Check::Ext(e) => yes(a.ext == *e),
            Check::Size(r) => yes(r.holds(a.size as f64)),
            Check::Age(r) => yes(r.holds(cx.now.saturating_sub(a.mtime) as f64)),
            Check::Res(r) => yes(known(Kind::Texture).is_some_and(|f| r.holds(f64::from(f.width.max(f.height))))),
            Check::Width(r) => yes(known(Kind::Texture).is_some_and(|f| r.holds(f64::from(f.width)))),
            Check::Height(r) => yes(known(Kind::Texture).is_some_and(|f| r.holds(f64::from(f.height)))),
            Check::Tris(r) => yes(known(Kind::Model).is_some_and(|f| r.holds(f64::from(f.width)))),
            Check::Seconds(r) => yes(known(Kind::Audio).is_some_and(|f| r.holds(f64::from(f.seconds)))),
            Check::Flag(flag) => yes(match *flag {
                Flag::Kind(k) => a.kind == k,
                Flag::Map(m) => a.map == m,
                Flag::AnyDataMap => !a.map.is_empty() && a.map != "color",
                Flag::Broken => (cx.facts)(a.key).is_some_and(|f| f.failed),
                Flag::Square => known(Kind::Texture).is_some_and(|f| f.width == f.height),
                Flag::PowerOfTwo => known(Kind::Texture).is_some_and(|f| f.width.is_power_of_two() && f.height.is_power_of_two()),
                Flag::Wide => known(Kind::Texture).is_some_and(|f| f.width > f.height),
                Flag::Tall => known(Kind::Texture).is_some_and(|f| f.height > f.width),
            }),
            Check::Any(all) => all.iter().filter_map(|c| c.fit(a, cx)).min(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    want: Vec<Check>,
    reject: Vec<Check>,
    now: u64,
}

/// One whitespace-separated piece of a query, with quotes understood.
struct Piece {
    negative: bool,
    key: Option<String>,
    value: String,
    quoted: bool,
}

fn pieces(text: &str) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() { chars.next(); continue }
        let negative = c == '-';
        if negative { chars.next(); }
        let (mut word, mut key, mut quoted, mut inside) = (String::new(), None, false, false);
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() && !inside { break }
            chars.next();
            match c {
                '"' => { inside = !inside; quoted = true; }
                ':' if !inside && key.is_none() && !word.is_empty() => key = Some(std::mem::take(&mut word).to_lowercase()),
                _ => word.push(c),
            }
        }
        if !word.is_empty() || key.is_some() { out.push(Piece { negative, key, value: word, quoted }); }
    }
    out
}

fn number(value: &str, units: &[(&str, f64)]) -> Option<f64> {
    let digits = value.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(value.len());
    let n: f64 = value[..digits].parse().ok()?;
    let unit = value[digits..].to_lowercase();
    units.iter().find(|(name, _)| *name == unit).map(|(_, scale)| n * scale)
}

/// `>1mb`, `<=500`, `1k..4k`, or a bare value: exact, or within a quarter of it if `loose`.
fn range(value: &str, units: &[(&str, f64)], loose: bool) -> Option<Range> {
    if let Some((low, high)) = value.split_once("..") {
        return Some(Range { min: number(low, units)?, max: number(high, units)? });
    }
    let parsed = |rest: &str| number(rest, units);
    if let Some(rest) = value.strip_prefix(">=") { return Some(Range { min: parsed(rest)?, max: f64::MAX }) }
    if let Some(rest) = value.strip_prefix("<=") { return Some(Range { min: 0.0, max: parsed(rest)? }) }
    if let Some(rest) = value.strip_prefix('>') { return Some(Range { min: parsed(rest)?.next_up(), max: f64::MAX }) }
    if let Some(rest) = value.strip_prefix('<') { return Some(Range { min: 0.0, max: parsed(rest)?.next_down() }) }
    let exact = parsed(value.strip_prefix('=').unwrap_or(value))?;
    Some(if loose { Range { min: exact * 0.75, max: exact * 1.25 } } else { Range { min: exact, max: exact } })
}

const BYTES: &[(&str, f64)] = &[("", 1.0), ("b", 1.0), ("k", 1024.0), ("kb", 1024.0), ("m", 1_048_576.0), ("mb", 1_048_576.0), ("g", 1_073_741_824.0), ("gb", 1_073_741_824.0)];
const PIXELS: &[(&str, f64)] = &[("", 1.0), ("px", 1.0), ("k", 1024.0)];
const COUNT: &[(&str, f64)] = &[("", 1.0), ("k", 1000.0)];
const SECONDS: &[(&str, f64)] = &[("", 1.0), ("s", 1.0), ("ms", 0.001), ("m", 60.0), ("min", 60.0), ("h", 3600.0)];
const AGE: &[(&str, f64)] = &[("", 86_400.0), ("h", 3600.0), ("d", 86_400.0), ("w", 604_800.0), ("mo", 2_592_000.0), ("y", 31_536_000.0)];

/// One value of a field. `None` when it isn't a complete value yet.
fn field_value(key: &str, value: &str) -> Option<Check> {
    let lowered = value.to_lowercase();
    if lowered.is_empty() { return None }
    Some(match key {
        "is" | "type" | "kind" | "map" => Check::Flag(resolve_flag(&lowered)?),
        "pack" => Check::Pack(lowered),
        "folder" | "in" | "path" => Check::Folder(lowered),
        "name" => Check::Name(lowered),
        "ext" | "format" => Check::Ext(crate::library::known_ext(lowered.trim_start_matches('.'))?),
        "size" => Check::Size(range(&lowered, BYTES, true)?),
        "age" => Check::Age(range(&lowered, AGE, true)?),
        "res" | "px" => Check::Res(range(&lowered, PIXELS, false)?),
        "w" | "width" => Check::Width(range(&lowered, PIXELS, false)?),
        "h" | "height" => Check::Height(range(&lowered, PIXELS, false)?),
        "tris" | "triangles" | "poly" | "polys" => Check::Tris(range(&lowered, COUNT, false)?),
        "dur" | "len" | "length" | "duration" | "time" => Check::Seconds(range(&lowered, SECONDS, true)?),
        _ => return None,
    })
}

const KEYS: &[&str] = &["is", "type", "kind", "map", "pack", "folder", "in", "path", "name", "ext", "format", "size", "age", "res", "px", "w", "width", "h", "height", "tris", "triangles", "poly", "polys", "dur", "len", "length", "duration", "time"];

fn one_of(mut checks: Vec<Check>) -> Option<Check> {
    match checks.len() {
        0 => None,
        1 => checks.pop(),
        _ => Some(Check::Any(checks)),
    }
}

impl Query {
    pub fn parse(text: &str, now: u64) -> Query {
        let mut q = Query { now, ..Query::default() };
        for piece in pieces(text) {
            let check = match &piece.key {
                Some(key) if KEYS.contains(&key.as_str()) => one_of(piece.value.split('|').filter_map(|v| field_value(key, v)).collect()),
                // Not a field after all: `foo:bar` is just text.
                Some(key) => Some(Check::Text(Text::new(&format!("{key}:{}", piece.value), piece.quoted))),
                None if piece.quoted => Some(Check::Text(Text::new(&piece.value, true))),
                None => one_of(piece.value.split('|').filter(|w| !w.is_empty()).map(|w| match w.strip_prefix('.').and_then(crate::library::known_ext) {
                    Some(ext) => Check::Ext(ext),
                    None => Check::Text(Text::new(w, false)),
                }).collect()),
            };
            let Some(mut check) = check else { continue };
            if piece.negative {
                // Excluding `door` shouldn't quietly exclude every gate too.
                if let Check::Text(t) = &mut check { t.related.clear(); }
                q.reject.push(check);
            } else {
                q.want.push(check);
            }
        }
        q
    }

    /// How well `a` fits, lower being better, or `None` if it doesn't.
    pub fn fit(&self, a: &Asset, facts: &dyn Fn(u64) -> Option<Facts>) -> Option<u32> {
        let cx = Context { now: self.now, facts };
        if self.reject.iter().any(|c| c.fit(a, &cx).is_some()) { return None }
        let mut total = 0;
        for check in &self.want { total += check.fit(a, &cx)?; }
        Some(total)
    }

    /// Whether the query names this format, which brings back models hidden behind a better one.
    pub fn asks_for_ext(&self, ext: &str) -> bool {
        fn names(c: &Check, ext: &str) -> bool {
            match c {
                Check::Ext(e) => *e == ext,
                Check::Any(all) => all.iter().any(|c| names(c, ext)),
                _ => false,
            }
        }
        self.want.iter().any(|c| names(c, ext))
    }

    /// Its answer depends on what thumbnailing has measured so far.
    pub fn needs_facts(&self) -> bool {
        self.want.iter().chain(&self.reject).any(Check::needs_facts)
    }

    pub fn has_words(&self) -> bool {
        self.want.iter().any(|c| matches!(c, Check::Text(_) | Check::Any(_)))
    }

    /// This query with misspelt words replaced by the nearest ones in the library, and those
    /// words, or `None` if nothing could be corrected.
    pub fn corrected(&self, vocabulary: &[(String, u32)]) -> Option<(Query, String)> {
        let mut fixed = self.clone();
        let mut words = Vec::new();
        for check in &mut fixed.want {
            let Check::Text(t) = check else { continue };
            if t.exact { continue }
            if let Some(word) = nearest(&t.raw, vocabulary) {
                words.push(word.to_owned());
                *t = Text::new(word, false);
            }
        }
        (!words.is_empty()).then(|| (fixed, words.join(" ")))
    }
}

/// The library word `term` was probably meant to be, unless it's already in there somewhere.
fn nearest<'a>(term: &str, vocabulary: &'a [(String, u32)]) -> Option<&'a str> {
    let allowed = match term.chars().count() {
        0..=3 => return None,
        4..=6 => 1,
        _ => 2,
    };
    if vocabulary.iter().any(|(w, _)| w.contains(term)) { return None }
    vocabulary.iter()
        .filter(|(w, _)| w.len().abs_diff(term.len()) <= allowed)
        .filter_map(|(w, count)| { let d = edits(term.as_bytes(), w.as_bytes()); (d <= allowed).then_some((d, std::cmp::Reverse(*count), w)) })
        .min()
        .map(|(_, _, w)| w.as_str())
}

/// Edit distance, a swap of neighbours counting once.
fn edits(a: &[u8], b: &[u8]) -> usize {
    let mut before = vec![0; b.len() + 1];
    let mut last: Vec<usize> = (0..=b.len()).collect();
    let mut row = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        row[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            row[j] = (last[j] + 1).min(row[j - 1] + 1).min(last[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] { row[j] = row[j].min(before[j - 2] + 1); }
        }
        std::mem::swap(&mut before, &mut last);
        std::mem::swap(&mut last, &mut row);
    }
    last[b.len()]
}

/// A row of the list under the search box.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    /// What the last piece of the query becomes.
    pub insert: String,
    pub hint: &'static str,
}

const FIELD_HELP: &[(&str, &str)] = &[
    ("is:", "model, normal, square, data"),
    ("pack:", "Pack name"),
    ("folder:", "Anywhere in the folders"),
    ("name:", "File name only"),
    ("ext:", "png, fbx, wav"),
    ("size:", "File size, >1mb"),
    ("res:", "Longest side, >=1024"),
    ("tris:", "Triangles, <500"),
    ("dur:", "Length, <2s"),
    ("age:", "Last changed, <30d"),
];

const EXAMPLES: &[(&str, &[(&str, &str)])] = &[
    ("size", &[(">1mb", "Over 1 MB"), ("<100kb", "Under 100 KB"), ("1mb..10mb", "Between")]),
    ("res", &[(">=2k", "2048 and up"), ("<=256", "256 or smaller"), ("1k", "Exactly 1024")]),
    ("tris", &[("<500", "Low poly"), (">10k", "Over 10,000"), ("1k..5k", "Between")]),
    ("dur", &[("<1s", "One-shots"), (">30s", "Long loops and music"), ("1s..5s", "Between")]),
    ("age", &[("<7d", "This week"), ("<30d", "This month"), (">1y", "Over a year ago")]),
];

/// What the last piece of `text` could become.
pub fn suggestions(text: &str, lib: &Library) -> Vec<Suggestion> {
    // Room for every `is:` value, which still fits the smallest window.
    const MOST: usize = 18;
    if text.trim().is_empty() {
        return FIELD_HELP.iter().map(|(k, hint)| Suggestion { insert: (*k).to_owned(), hint }).collect();
    }
    if text.ends_with(char::is_whitespace) || text.matches('"').count() % 2 == 1 { return Vec::new() }
    let last = text.rsplit(char::is_whitespace).next().unwrap_or("");
    let (sign, last) = last.strip_prefix('-').map_or(("", last), |rest| ("-", rest));
    let Some((key, typed)) = last.split_once(':') else {
        if last.len() < 2 { return Vec::new() }
        let typed = last.to_lowercase();
        return FIELD_HELP.iter().filter(|(k, _)| k.starts_with(&typed)).map(|(k, hint)| Suggestion { insert: format!("{sign}{k}"), hint }).collect();
    };
    let key = key.to_lowercase();
    let typed_lower = typed.trim_start_matches('"').to_lowercase();
    // Lists after `|` continue from what's been typed so far.
    let (done, typed_lower) = typed_lower.rsplit_once('|').map_or((String::new(), typed_lower.clone()), |(a, b)| (format!("{a}|"), b.to_owned()));
    let row = |value: &str, hint: &'static str| {
        let value = if value.contains(' ') { format!("\"{value}\"") } else { value.to_owned() };
        Suggestion { insert: format!("{sign}{key}:{done}{value} "), hint }
    };
    let mut out: Vec<Suggestion> = match key.as_str() {
        "is" | "type" | "kind" | "map" => FLAGS.iter().filter(|(words, _)| words.iter().any(|w| w.starts_with(&typed_lower))).map(|(words, flag)| row(words[0], flag_hint(*flag))).collect(),
        "ext" | "format" => crate::library::known_exts().filter(|e| e.starts_with(&typed_lower)).map(|e| row(e, "Format")).collect(),
        "pack" => lib.packs.iter().filter(|p| p.name.to_lowercase().contains(&typed_lower)).map(|p| row(&p.name, "Pack")).collect(),
        _ => EXAMPLES.iter().find(|(k, _)| *k == key).map_or_else(Vec::new, |(_, rows)| rows.iter().filter(|(v, _)| v.starts_with(&typed_lower)).map(|(v, hint)| row(v, hint)).collect()),
    };
    out.truncate(MOST);
    // Nothing to add to something already complete.
    if out.len() == 1 && out[0].insert.trim_end() == format!("{sign}{last}") { out.clear(); }
    out
}

fn flag_hint(flag: Flag) -> &'static str {
    match flag {
        Flag::Kind(Kind::Model) => "3D models",
        Flag::Kind(Kind::Texture) => "Images",
        Flag::Kind(Kind::Audio) => "Sounds and music",
        Flag::Map("color") => "Base colour maps",
        Flag::Map(_) => "Texture map",
        Flag::AnyDataMap => "Normal, roughness and other maps",
        Flag::Square => "Width equals height",
        Flag::PowerOfTwo => "Sides are powers of two",
        Flag::Wide => "Wider than tall",
        Flag::Tall => "Taller than wide",
        Flag::Broken => "Files that can't be previewed",
    }
}

/// `text` with its last piece replaced.
pub fn apply(text: &str, suggestion: &Suggestion) -> String {
    let start = text.rfind(char::is_whitespace).map_or(0, |i| i + text[i..].chars().next().map_or(1, char::len_utf8));
    format!("{}{}", &text[..start], suggestion.insert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Entry, Filter, Sort, search_with};
    use std::path::PathBuf;

    fn entry(rel: &str, size: u64, mtime: u64) -> Entry {
        Entry { rel: rel.into(), size, mtime }
    }

    fn lib() -> Library {
        Library::build(PathBuf::from("C:/lib"), vec![
            entry("Mega/Tex/brick_wall.png", 2_000_000, 900),
            entry("Mega/Tex/brick_wall_normal.png", 3_000_000, 900),
            entry("Mega/Tex/T_Brick_N.png", 500, 900),
            entry("Mega/Tex/T_Brick_D.png", 500, 900),
            entry("Mega/Tex/Wall_Height.png", 500, 900),
            entry("Mega/Tex/Wall_Base_color.png", 500, 900),
            entry("Mega/Other/rusty_metal_4.png", 500, 900),
            entry("Mega/Other/metal_6.png", 500, 900),
            entry("Mega/Other/floor_3_metal.png", 500, 900),
            entry("Mega/Other/floor_3_metal_1.png", 500, 900),
            entry("Mega/Tex/Material_Roughness.png", 500, 900),
            entry("Mega/Tex/outdoor_sign.png", 500, 100),
            entry("Mega/Models/door_2.glb", 900, 900),
            entry("Mega/Models/door_2.fbx", 900, 900),
            entry("Mega/Models/oak gate.obj", 900, 900),
            entry("Mega/Models/woodDoor.obj", 900, 900),
            entry("Guns/pistol_mp_1.fbx", 900, 900),
            entry("Guns/Doors/hinge.fbx", 900, 900),
            entry("Sfx/barrel_hit.wav", 4000, 900),
            entry("Sfx/long ambience.ogg", 9_000_000, 900),
            entry("loose.png", 10, 900),
        ])
    }

    fn facts_of(l: &Library) -> impl Fn(u64) -> Option<Facts> + '_ {
        move |key| {
            let a = l.assets.iter().find(|a| a.key == key)?;
            Some(match a.name() {
                "brick_wall.png" => Facts { width: 2048, height: 2048, ..Facts::default() },
                "brick_wall_normal.png" => Facts { width: 1000, height: 512, ..Facts::default() },
                "door_2.glb" => Facts { width: 300, ..Facts::default() },
                "pistol_mp_1.fbx" => Facts { width: 9000, ..Facts::default() },
                "barrel_hit.wav" => Facts { seconds: 0.8, ..Facts::default() },
                "long ambience.ogg" => Facts { seconds: 95.0, ..Facts::default() },
                "T_Brick_N.png" => Facts { failed: true, ..Facts::default() },
                _ => return None,
            })
        }
    }

    fn find(text: &str) -> Vec<String> {
        let l = lib();
        let facts = facts_of(&l);
        let r = search_with(&l, &Filter { text: text.into(), ..Filter::default() }, &facts);
        r.ids.iter().map(|i| l.assets[*i as usize].name().to_owned()).collect()
    }

    fn set(names: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = names.iter().map(|s| (*s).to_owned()).collect();
        v.sort();
        v
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn words_match_across_separators_and_case() {
        assert_eq!(find("wood door"), ["woodDoor.obj"]);
        assert_eq!(find("woodDoor"), ["woodDoor.obj"]);
        assert_eq!(find("\"brick wall\""), find("\"brick_wall\""));
    }

    #[test]
    fn quotes_mean_whole_words_and_nothing_related() {
        assert_eq!(sorted(find("door")), set(&["door_2.glb", "hinge.fbx", "oak gate.obj", "outdoor_sign.png", "woodDoor.obj"]));
        assert_eq!(sorted(find("\"door\"")), set(&["door_2.glb", "woodDoor.obj"]));
        assert_eq!(find("\"brick wall\" -normal"), ["brick_wall.png"]);
    }

    #[test]
    fn related_words_come_after_literal_ones() {
        let found = find("door");
        assert_eq!(found[..2], ["door_2.glb".to_owned(), "woodDoor.obj".to_owned()], "{found:?}");
        assert_eq!(found.last().unwrap(), "oak gate.obj", "{found:?}");
        assert_eq!(find("pistol"), ["pistol_mp_1.fbx"]);
        assert!(find("gun").contains(&"pistol_mp_1.fbx".to_owned()));
    }

    #[test]
    fn excluding_a_word_leaves_its_relatives() {
        assert_eq!(find("-door oak"), ["oak gate.obj"]);
    }

    #[test]
    fn either_or_and_exclusions_work_on_words_and_fields() {
        assert_eq!(sorted(find("pistol|barrel")), set(&["barrel_hit.wav", "pistol_mp_1.fbx"]));
        assert_eq!(sorted(find("is:audio|model -is:model")), set(&["barrel_hit.wav", "long ambience.ogg"]));
        assert_eq!(sorted(find("pack:sfx|guns -ext:wav -ext:ogg")), set(&["hinge.fbx", "pistol_mp_1.fbx"]));
    }

    #[test]
    fn where_a_word_is_can_be_chosen() {
        assert_eq!(find("name:door is:model -ext:obj"), ["door_2.glb"]);
        assert_eq!(find("folder:doors"), ["hinge.fbx"]);
        assert_eq!(find("pack:\"loose files\""), ["loose.png"]);
    }

    #[test]
    fn asking_for_a_format_brings_back_the_hidden_copy() {
        assert_eq!(find("door_2"), ["door_2.glb"]);
        assert_eq!(find("door_2 ext:fbx"), ["door_2.fbx"]);
        assert_eq!(find("door_2 .fbx"), ["door_2.fbx"]);
    }

    #[test]
    fn texture_maps_are_read_from_names() {
        assert_eq!(sorted(find("is:normal")), set(&["brick_wall_normal.png", "T_Brick_N.png"]));
        assert_eq!(sorted(find("brick -is:data")), set(&["brick_wall.png", "T_Brick_D.png"]));
        assert_eq!(find("is:roughness"), ["Material_Roughness.png"]);
        assert_eq!(find("is:height"), ["Wall_Height.png"], "beside another map of the wall");
        assert!(find("is:metal").is_empty(), "a picture of metal is no metalness map");
        assert_eq!(sorted(find("metal -is:data")), set(&["floor_3_metal.png", "floor_3_metal_1.png", "metal_6.png", "rusty_metal_4.png"]), "nor are two variants a set");
    }

    #[test]
    fn sizes_and_ages_filter() {
        assert_eq!(sorted(find("size:>1mb is:texture")), set(&["brick_wall.png", "brick_wall_normal.png"]));
        assert_eq!(find("size:1mb..2.5mb"), ["brick_wall.png"]);
        let l = lib();
        let q = Query::parse("age:<1d", 950);
        let young: Vec<_> = l.assets.iter().filter(|a| q.fit(a, &|_| None).is_some()).map(|a| a.name()).collect();
        assert_eq!(young.len(), l.assets.len(), "all within a day of 950");
        let q = Query::parse("age:>400", 200_000_000);
        assert_eq!(l.assets.iter().filter(|a| q.fit(a, &|_| None).is_some()).count(), l.assets.len());
    }

    #[test]
    fn measured_facts_filter_and_unmeasured_assets_stay_out() {
        assert_eq!(find("res:>=2k"), ["brick_wall.png"]);
        assert_eq!(find("w:1000 h:512"), ["brick_wall_normal.png"]);
        assert_eq!(find("is:square"), ["brick_wall.png"]);
        assert_eq!(find("is:wide"), ["brick_wall_normal.png"]);
        assert_eq!(find("tris:<500"), ["door_2.glb"]);
        assert_eq!(find("tris:>1k"), ["pistol_mp_1.fbx"]);
        assert_eq!(find("dur:<2s"), ["barrel_hit.wav"]);
        assert_eq!(find("dur:>1m"), ["long ambience.ogg"]);
        assert_eq!(find("is:broken"), ["T_Brick_N.png"]);
    }

    #[test]
    fn a_field_being_typed_changes_nothing() {
        let everything = find("");
        for partial in ["size:", "size:>", "res:>=", "is:", "ext:", "tris:1.5x", "pack:", "-"] {
            assert_eq!(find(partial), everything, "{partial}");
        }
        assert_eq!(find("is:mod"), find("is:model"), "an unfinished value is taken as the one it starts");
    }

    #[test]
    fn something_that_only_looks_like_a_field_is_text() {
        assert!(find("foo:bar").is_empty());
    }

    #[test]
    fn a_typo_is_corrected_only_when_nothing_matches() {
        let l = lib();
        let run = |text: &str| search_with(&l, &Filter { text: text.into(), ..Filter::default() }, &|_| None);
        let fixed = run("barel");
        assert_eq!(fixed.corrected.as_deref(), Some("barrel"));
        assert_eq!(fixed.ids.len(), 1);
        let fixed = run("pistlo");
        assert_eq!(fixed.corrected.as_deref(), Some("pistol"), "a swap counts once");
        assert_eq!(run("barrel").corrected, None);
        let nothing = run("xylophone");
        assert!(nothing.ids.is_empty() && nothing.corrected.is_none());
        assert!(run("\"barel\"").ids.is_empty(), "quotes mean exactly that");
    }

    #[test]
    fn best_fits_lead_only_under_the_default_order() {
        let l = lib();
        let names = |sort| -> Vec<String> {
            search_with(&l, &Filter { text: "door".into(), sort, ..Filter::default() }, &|_| None).ids.iter().map(|i| l.assets[*i as usize].name().to_owned()).collect()
        };
        assert_eq!(names(Sort::Pack).last().unwrap(), "oak gate.obj", "a related word trails");
        assert_eq!(names(Sort::Name), ["door_2.glb", "hinge.fbx", "oak gate.obj", "outdoor_sign.png", "woodDoor.obj"], "a chosen order is kept");
    }

    #[test]
    fn suggestions_complete_the_last_piece() {
        let l = lib();
        let inserts = |text: &str| -> Vec<String> { suggestions(text, &l).into_iter().map(|s| s.insert).collect() };
        assert_eq!(inserts("").len(), FIELD_HELP.len());
        assert_eq!(inserts("door si"), ["size:"]);
        assert_eq!(inserts("is:nor"), ["is:normal "]);
        assert_eq!(inserts("-is:nor"), ["-is:normal "]);
        assert_eq!(inserts("pack:gu"), ["pack:Guns "]);
        assert_eq!(inserts("pack:loo").len(), 1);
        assert_eq!(inserts("ext:f"), ["ext:fbx ", "ext:flac "]);
        assert_eq!(inserts("is:audio|mo"), ["is:audio|model "]);
        assert!(inserts("door ").is_empty(), "a finished word has nothing to add");
        assert!(inserts("door").is_empty());
        assert!(inserts("is:normal").is_empty(), "already complete");
        assert_eq!(inserts("size:>").len(), 1);
    }

    #[test]
    fn a_suggestion_replaces_only_the_last_piece() {
        let s = Suggestion { insert: "is:normal ".into(), hint: "" };
        assert_eq!(apply("brick is:nor", &s), "brick is:normal ");
        assert_eq!(apply("is:nor", &s), "is:normal ");
        let pack = Suggestion { insert: "pack:\"PSX Nature\" ".into(), hint: "" };
        assert_eq!(apply("tree pack:ps", &pack), "tree pack:\"PSX Nature\" ");
    }

    #[test]
    fn a_quoted_pack_with_spaces_still_parses_after_being_suggested() {
        assert_eq!(pieces("pack:\"PSX Nature\" tree").len(), 2);
        let q = Query::parse("pack:\"PSX Nature\"", 0);
        assert_eq!(q.want, [Check::Pack("psx nature".into())]);
    }
}
