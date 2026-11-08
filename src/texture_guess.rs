//! Finds a colour texture for a model that doesn't link one, or links one that's gone.
//!
//! Packs rarely agree on how they lay this out. A model might sit beside `Material_Base_color.png`,
//! name `akm_default.png` in a sibling folder, or have a lone `txtDamaged2.png` with nothing in
//! common with its name. So nearby images are scored on how close they are and how well their
//! names fit, and data maps (normals, roughness and so on) are ruled out first. A model with no
//! convincing candidate stays untextured: wrong is worse than plain.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::library::natural;
use crate::model::{Image, Model, decode_image};

/// What a file says about one of its materials, for the guess to work from.
#[derive(Clone, Debug, Default)]
pub struct MaterialSource {
    pub name: String,
    /// The texture it names, if that couldn't be found.
    pub reference: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    /// Beside the model.
    Same,
    /// In a folder under the model that's named for textures.
    TextureChild,
    /// In any other folder under the model.
    Child,
    /// In a texture folder beside one of the model's parent folders.
    AncestorTextures,
    /// Directly in a parent folder.
    Ancestor,
}

impl Place {
    fn score(self) -> f32 {
        match self {
            Place::Same => 30.0,
            Place::TextureChild => 26.0,
            Place::Child => 14.0,
            Place::AncestorTextures => 12.0,
            Place::Ancestor => 4.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Colour,
    Data,
    Unknown,
}

#[derive(Clone, Debug)]
struct Candidate {
    path: PathBuf,
    place: Place,
    stem: String,
    tokens: Vec<String>,
    /// The words left once prefixes, colour words and sizes are gone: what it's a texture *of*.
    core: Vec<String>,
    role: Role,
}

/// Words at the end of a name that always mean it isn't a colour map.
const DATA_WORDS: &[&str] = &[
    "normal", "normals", "nrm", "norm", "nor", "bump", "displacement", "disp", "roughness", "rgh", "glossiness", "metallic", "metalness", "ao",
    "occlusion", "ambientocclusion", "specular", "emission", "emissive", "orm", "arm", "rma", "mra", "curvature", "thickness", "cavity", "opacity",
];
/// Words that mean it only beside a file for the same surface: `brick_rough_blue` and
/// `wall_metal` are colour textures, `wall_basecolor` next to `wall_metal` makes it a mask.
const WEAK_DATA_WORDS: &[&str] = &["rough", "metal", "gloss", "height", "mask", "alpha", "spec", "glow", "emit", "n", "r", "m", "s", "e"];
const COLOUR_WORDS: &[&str] = &["albedo", "diffuse", "diff", "color", "colour", "col", "basecolor", "base", "bc"];
/// Words that say nothing about what a texture depicts.
const FILLER_WORDS: &[&str] = &["t", "tx", "txt", "tex", "texture", "textures", "m", "mat", "material", "materials", "default", "defaultmaterial", "base", "main", "sm", "sk", "s", "map", "img", "image", "uv", "lambert", "phong", "standard", "surface"];
const PREFERRED_WORDS: &[&str] = &["default", "clean", "main", "base"];
const WORN_WORDS: &[&str] = &["bloody", "blood", "rusted", "rusty", "dirty", "damaged", "old", "worn", "broken", "burnt", "dead"];
const TEXTURE_FOLDER_WORDS: &[&str] = &["texture", "textures", "tex", "material", "materials", "maps", "map", "images", "img", "skin", "skins", "color", "colour", "albedo", "diffuse"];

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "tga", "bmp", "webp"];
const PARENT_LEVELS: usize = 4;
/// How many of the best candidates may be opened before giving up on the rest.
const MAX_OPENED: usize = 6;

/// Splits on punctuation, camelCase and letter/digit boundaries, lowercased.
fn tokens_of(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut previous: Option<char> = None;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !word.is_empty() { out.push(std::mem::take(&mut word)); }
            previous = None;
            continue;
        }
        let boundary = previous.is_some_and(|p| (p.is_lowercase() && c.is_uppercase()) || (p.is_ascii_digit() != c.is_ascii_digit()));
        if boundary && !word.is_empty() { out.push(std::mem::take(&mut word)); }
        word.extend(c.to_lowercase());
        previous = Some(c);
    }
    if !word.is_empty() { out.push(word); }
    out
}

fn is_number(word: &str) -> bool {
    word.chars().all(|c| c.is_ascii_digit())
}

fn is_texture_size(word: &str) -> bool {
    word.parse::<u32>().is_ok_and(|n| n >= 64 && n.is_power_of_two())
}

/// The name's last word that isn't a number, with the word before it.
fn tail(tokens: &[String]) -> (Option<&str>, Option<&str>) {
    let mut words = tokens.iter().rev().filter(|t| !is_number(t)).map(String::as_str);
    (words.next(), words.next())
}

fn role_of(tokens: &[String]) -> Role {
    let (last, before) = tail(tokens);
    let Some(last) = last else { return Role::Unknown };
    if DATA_WORDS.contains(&last) { return Role::Data; }
    if COLOUR_WORDS.contains(&last) || (last == "colors") || (before == Some("base") && last == "color") { return Role::Colour; }
    Role::Unknown
}

/// The words that describe what's depicted.
fn core_of(tokens: &[String]) -> Vec<String> {
    tokens.iter().filter(|t| !FILLER_WORDS.contains(&t.as_str()) && !COLOUR_WORDS.contains(&t.as_str()) && !is_number(t)).cloned().collect()
}

/// A model or material name reduced the same way.
fn subject_of(name: &str) -> Vec<String> {
    core_of(&tokens_of(name))
}

impl Candidate {
    fn new(path: PathBuf, place: Place) -> Candidate {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let tokens = tokens_of(&stem);
        let core = core_of(&tokens);
        let role = role_of(&tokens);
        Candidate { path, place, stem, tokens, core, role }
    }
}

/// A name with a weak ending is a data map when another file shares the rest of its name.
fn demote_weak_data(candidates: &mut [Candidate]) {
    let prefix = |c: &Candidate| -> Option<Vec<String>> {
        let last = c.tokens.iter().rposition(|t| !is_number(t))?;
        Some(c.tokens[..last].to_vec())
    };
    let prefixes: Vec<Option<Vec<String>>> = candidates.iter().map(prefix).collect();
    // `Material_Base_color` partners `Material_Height`, so a partner's own colour words don't count.
    let bare = |tokens: &Vec<String>| -> Vec<String> {
        let end = tokens.iter().rposition(|t| !COLOUR_WORDS.contains(&t.as_str())).map_or(0, |at| at + 1);
        tokens[..end].to_vec()
    };
    for i in 0..candidates.len() {
        let Some(own) = &prefixes[i] else { continue };
        let last = tail(&candidates[i].tokens).0.unwrap_or("").to_owned();
        if candidates[i].role != Role::Unknown || !WEAK_DATA_WORDS.contains(&last.as_str()) { continue }
        // An empty prefix (a file called `rough.png`) means nothing was left to compare.
        let paired = !own.is_empty() && prefixes.iter().enumerate().any(|(j, other)| {
            j != i && other.as_ref().is_some_and(|other| other == own || bare(other) == *own)
        });
        if paired { candidates[i].role = Role::Data; }
    }
}

fn is_image(path: &Path) -> bool {
    path.extension().is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn folder_kind(name: &str) -> (bool, bool) {
    let tokens = tokens_of(name);
    let texture = tokens.iter().any(|t| TEXTURE_FOLDER_WORDS.contains(&t.as_str()));
    let data = role_of(&tokens) == Role::Data || tokens.iter().any(|t| DATA_WORDS.contains(&t.as_str()));
    (texture, data)
}

fn images_in(dir: &Path, place: Place, into: &mut Vec<Candidate>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() && is_image(&path) { into.push(Candidate::new(path, place)); }
    }
}

fn subfolders(dir: &Path) -> Vec<(PathBuf, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.filter_map(Result::ok).filter(|e| e.path().is_dir()).filter_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let (texture, data) = folder_kind(&name);
        (!data).then(|| (e.path(), texture))
    }).collect()
}

fn gather(model_dir: &Path, library: &Path) -> Vec<Candidate> {
    let mut found = Vec::new();
    images_in(model_dir, Place::Same, &mut found);
    for (dir, texture) in subfolders(model_dir) {
        images_in(&dir, if texture { Place::TextureChild } else { Place::Child }, &mut found);
    }
    // Far enough for `Pack/Models/other-formats/OBJ/Props/x.obj` to reach `Pack/Textures`.
    for parent in model_dir.ancestors().skip(1).take(PARENT_LEVELS).take_while(|p| p.starts_with(library)) {
        images_in(parent, Place::Ancestor, &mut found);
        for (dir, texture) in subfolders(parent) {
            if texture && dir != model_dir { images_in(&dir, Place::AncestorTextures, &mut found); }
        }
    }
    demote_weak_data(&mut found);
    found
}

/// Two words that are the same, or one a long part of the other (`campaxe`, `axe` won't, but
/// `zombienaked` and `zombie` will), or a long word with its middle dropped (`metalbat` for
/// `metalbaseballbat`).
fn words_match(a: &str, b: &str) -> bool {
    if a == b { return true }
    if a.len() < 4 || b.len() < 4 { return false }
    if a.contains(b) || b.contains(a) { return true }
    let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
    let (front, back) = (short.bytes().zip(long.bytes()).take_while(|(x, y)| x == y).count(),
        short.bytes().rev().zip(long.bytes().rev()).take_while(|(x, y)| x == y).count());
    short.len() >= 6 && front >= 3 && back >= 3 && front + back >= short.len()
}

/// How well a candidate's subject fits a name: the share of its own words that fit, and the
/// share of the name's words that it covers.
fn fit(core: &[String], subject: &[String]) -> (f32, f32) {
    if core.is_empty() || subject.is_empty() { return (0.0, 0.0); }
    let own = core.iter().filter(|c| subject.iter().any(|s| words_match(c, s))).count() as f32 / core.len() as f32;
    let covers = subject.iter().filter(|s| core.iter().any(|c| words_match(c, s))).count() as f32 / subject.len() as f32;
    (own, covers)
}

struct Ask<'a> {
    model: &'a str,
    material: &'a str,
    reference: Option<&'a str>,
    /// Another material here has a real texture, so this one may be flat on purpose.
    linked_elsewhere: bool,
}

/// The candidate's score for this material, or None if it shouldn't be used at all.
/// The bare file name in a reference that may be a stale path from someone else's machine.
fn file_stem_of(reference: &str) -> String {
    let name = reference.trim().replace('\\', "/");
    let name = name.rsplit('/').next().unwrap_or("");
    name.rsplit_once('.').map_or(name, |(stem, _)| stem).trim().to_owned()
}

/// Whether the file's name alone settles that it's this texture, so its pixels needn't argue:
/// a cobweb or a steel blade is honestly grey.
fn named_outright(c: &Candidate, ask: &Ask<'_>) -> bool {
    let reference = ask.reference.map(file_stem_of).unwrap_or_default();
    c.role == Role::Colour || [ask.model, ask.material, &reference].iter().any(|n| !n.is_empty() && c.stem.eq_ignore_ascii_case(n))
}

fn score(c: &Candidate, ask: &Ask<'_>, alone_in_folder: bool) -> Option<f32> {
    if c.role == Role::Data { return None; }

    // The file names this texture, even if the path to it is stale.
    if ask.reference.is_some_and(|r| c.stem.eq_ignore_ascii_case(&file_stem_of(r))) { return Some(1000.0 + c.place.score()); }

    let model = subject_of(ask.model);
    let material = subject_of(ask.material);
    let reference = ask.reference.map(|r| subject_of(r.replace('\\', "/").rsplit('/').next().unwrap_or(""))).unwrap_or_default();
    let (mut own, mut covers) = (0.0f32, 0.0f32);
    for subject in [&model, &material, &reference] {
        let (o, c) = fit(&c.core, subject);
        if o > own || (o == own && c > covers) { (own, covers) = (o, c); }
    }
    let unnamed = c.core.is_empty();
    let matched = own >= 1.0 || (own >= 0.5 && covers >= 0.5);

    let near = matches!(c.place, Place::Same | Place::TextureChild | Place::Child);
    let accepted = if ask.linked_elsewhere {
        // Part of the model is textured, so only a clear match for this part may join it.
        matched && !subject_of(ask.material).is_empty() && fit(&c.core, &material).0 >= 0.5
    } else if matches!(c.place, Place::Ancestor | Place::AncestorTextures) {
        // Further away, the name has to do the work: a shared folder proves nothing.
        own >= 1.0 && covers >= 0.5
    } else if matched {
        true
    } else if unnamed {
        near
    } else {
        // A name with nothing in common, but nothing else here it could be.
        alone_in_folder && c.place == Place::Same
    };
    if !accepted { return None; }

    let mut total = c.place.score() + 40.0 * own + 30.0 * covers;
    if c.role == Role::Colour { total += 10.0; }
    if unnamed { total += 6.0; }
    if c.tokens.iter().any(|t| PREFERRED_WORDS.contains(&t.as_str())) { total += 6.0; }
    let wanted_worn = model.iter().any(|m| WORN_WORDS.contains(&m.as_str()));
    if !wanted_worn && c.tokens.iter().any(|t| WORN_WORDS.contains(&t.as_str())) { total -= 12.0; }
    // Words the model doesn't have are a different variant of it (`shirt_yellow` for `shirt`).
    let extra = c.core.iter().filter(|w| !model.iter().chain(&material).any(|s| words_match(w, s))).count();
    total -= 8.0 * extra as f32;
    // The biggest copy of the same texture, when a pack ships several.
    if let Some(size) = c.tokens.iter().filter(|t| is_texture_size(t)).filter_map(|t| t.parse::<f32>().ok()).next() { total += size.log2() / 2.0; }
    Some(total)
}

/// Candidates that could serve this material, best first.
fn ranked(candidates: &[Candidate], ask: &Ask<'_>) -> Vec<usize> {
    let beside = candidates.iter().filter(|c| c.place == Place::Same && c.role != Role::Data).count();
    let mut scored: Vec<(usize, f32)> = candidates.iter().enumerate()
        .filter_map(|(i, c)| score(c, ask, beside == 1).map(|s| (i, s)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal).then_with(|| natural(&candidates[a.0].stem, &candidates[b.0].stem)));
    scored.into_iter().map(|(i, _)| i).collect()
}

/// Whether the pixels are plausibly a colour texture. Names lie, or say nothing.
fn looks_like_colour(image: &Image, named_colour: bool) -> bool {
    let (mut sums, mut grey, mut count) = ([0u64; 3], 0u64, 0u64);
    let step = ((image.rgba.len() / 4) / 4096).max(1) * 4;
    for px in image.rgba.chunks_exact(4).step_by(step / 4) {
        if px[3] < 16 { continue }
        for c in 0..3 { sums[c] += u64::from(px[c]); }
        if px[0].abs_diff(px[1]) <= 2 && px[1].abs_diff(px[2]) <= 2 { grey += 1; }
        count += 1;
    }
    if count == 0 { return false; }
    let [r, g, b] = sums.map(|s| s as f32 / count as f32);
    // Tangent-space normals average out to a pale blue-violet.
    let normal_map = (r - 128.0).abs() < 24.0 && (g - 128.0).abs() < 24.0 && b > 190.0;
    if normal_map { return false; }
    // Perfectly grey images are roughness, height or AO, unless the name says otherwise.
    named_colour || (grey as f32) < count as f32 * 0.995
}

/// Gives every untextured material a texture from around the model, where one can be found.
pub fn fill(path: &Path, library: &Path, model: &mut Model) {
    let mut wanted: Vec<u32> = model.parts.iter().map(|p| p.material).filter(|m| model.materials[*m as usize].image.is_none()).collect();
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.is_empty() { return }
    let linked_elsewhere = model.parts.iter().any(|p| model.materials[p.material as usize].image.is_some());
    let Some(dir) = path.parent() else { return };
    let candidates = gather(dir, library);
    if candidates.is_empty() { return }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

    let mut opened: HashMap<usize, Option<u32>> = HashMap::new();
    for material in wanted {
        let source = model.sources.get(material as usize).cloned().unwrap_or_default();
        let ask = Ask { model: &stem, material: &source.name, reference: source.reference.as_deref(), linked_elsewhere };
        for index in ranked(&candidates, &ask).into_iter().take(MAX_OPENED) {
            let candidate = &candidates[index];
            let image = *opened.entry(index).or_insert_with(|| {
                let image = std::fs::read(&candidate.path).ok().and_then(|b| decode_image(&b))?;
                if !looks_like_colour(&image, named_outright(candidate, &ask)) { return None; }
                model.images.push(image);
                model.guessed.push(candidate.path.clone());
                Some(model.images.len() as u32 - 1)
            });
            if let Some(image) = image {
                let slot = &mut model.materials[material as usize];
                slot.image = Some(image);
                slot.color = [1.0; 4];
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(files: &[(&str, Place)]) -> Vec<Candidate> {
        let mut all: Vec<Candidate> = files.iter().map(|(f, p)| Candidate::new(PathBuf::from(f), *p)).collect();
        demote_weak_data(&mut all);
        all
    }

    fn best(files: &[(&str, Place)], model: &str, material: &str) -> Option<String> {
        let all = set(files);
        let ask = Ask { model, material, reference: None, linked_elsewhere: false };
        ranked(&all, &ask).first().map(|i| all[*i].stem.clone())
    }

    use Place::*;

    #[test]
    fn names_split_the_way_people_write_them() {
        assert_eq!(tokens_of("T_GhoulA_1024"), ["t", "ghoul", "a", "1024"]);
        assert_eq!(tokens_of("Material_Base_color2"), ["material", "base", "color", "2"]);
        assert_eq!(tokens_of("txtShirtYellow"), ["txt", "shirt", "yellow"]);
    }

    #[test]
    fn data_maps_are_never_picked_however_much_their_name_fits() {
        let files = [("Material_Height.png", Same), ("Material_Mixed_AO.png", Same), ("Material_Normal.png", Same), ("Material_Roughness.png", Same), ("Material_Metallic.png", Same), ("Material_Base_color.png", Same)];
        assert_eq!(best(&files, "whisky", "Material").as_deref(), Some("Material_Base_color"));
        assert_eq!(best(&files[..5], "whisky", "Material"), None, "with only data maps, stay untextured");
        assert_eq!(best(&[("crate_normal.png", Same)], "crate", ""), None);
    }

    #[test]
    fn words_only_mean_a_data_map_at_the_end_of_the_name_or_beside_a_partner() {
        assert_eq!(best(&[("brick_rough_blue.png", Same)], "wall", ""), Some("brick_rough_blue".to_owned()), "rough is part of its name");
        assert_eq!(best(&[("metal_cabinet.png", Same)], "cabinet", ""), Some("metal_cabinet".to_owned()));
        assert_eq!(best(&[("wall_metal.png", Same)], "wall", ""), Some("wall_metal".to_owned()), "alone, it's a metal wall");
        let pair = [("wall_metal.png", Same), ("wall_basecolor.png", Same)];
        assert_eq!(best(&pair, "wall", "").as_deref(), Some("wall_basecolor"), "beside a colour map, it's a mask");
        assert!(set(&pair)[0].role == Role::Data);
    }

    #[test]
    fn the_models_own_name_beats_variants_and_neighbours() {
        let files = [("akm_blackout.png", AncestorTextures), ("akm_default.png", AncestorTextures), ("akm_rusted.png", AncestorTextures), ("p226_default.png", AncestorTextures), ("r870_default.png", AncestorTextures)];
        assert_eq!(best(&files, "akm", "").as_deref(), Some("akm_default"));
        assert_eq!(best(&files, "mp7a1", ""), None, "another weapon's texture is not a guess, it's wrong");
    }

    #[test]
    fn a_variant_the_name_asks_for_wins() {
        let files = [("campaxe_bloody.png", AncestorTextures), ("campaxe_default.png", AncestorTextures)];
        assert_eq!(best(&files, "campaxe", "").as_deref(), Some("campaxe_default"));
        assert_eq!(best(&files, "campaxe_bloody", "").as_deref(), Some("campaxe_bloody"));
        let shirts = [("txtShirt.png", Same), ("txtShirtYellow.png", Same)];
        assert_eq!(best(&shirts, "zombieShirt", "").as_deref(), Some("txtShirt"));
        assert_eq!(best(&shirts, "zombieShirtYellow", "").as_deref(), Some("txtShirtYellow"));
    }

    #[test]
    fn a_generic_map_beside_the_model_is_trusted() {
        assert_eq!(best(&[("Material_Base_color2.png", Same)], "poleaxe_02", "Material").as_deref(), Some("Material_Base_color2"));
        assert_eq!(best(&[("_txt.png", Same)], "zombie", "").as_deref(), Some("_txt"));
        assert_eq!(best(&[("base.png", Same)], "zombieNaked", "").as_deref(), Some("base"));
    }

    #[test]
    fn the_only_image_beside_a_model_is_its_texture_whatever_it_is_called() {
        assert_eq!(best(&[("txtDamaged2.png", Same)], "ZombieBadlyDamaged", "").as_deref(), Some("txtDamaged2"));
        assert_eq!(best(&[("foo.png", Same)], "barrel", "").as_deref(), Some("foo"));
        assert_eq!(best(&[("foo.png", Same), ("qux.png", Same)], "barrel", ""), None, "two unrelated images is a coin toss");
    }

    #[test]
    fn variants_in_subfolders_and_sizes_resolve_sensibly() {
        let faces = [("Face/base.png", Child), ("Face1/base.png", Child), ("Face2/base.png", Child)];
        assert!(best(&faces, "zombieNakedFace", "").is_some());
        let ghoul = [("T_GhoulA_512.png", TextureChild), ("T_GhoulA_1024.png", TextureChild), ("T_GhoulB_1024.png", TextureChild), ("T_GhoulB_512.png", TextureChild)];
        assert_eq!(best(&ghoul, "SK_Ghoul", "").as_deref(), Some("T_GhoulA_1024"));
    }

    #[test]
    fn an_empty_folder_borrows_nothing_from_its_neighbours() {
        assert_eq!(best(&[("Material_Base_color.png", AncestorTextures), ("door_old.png", Ancestor)], "chain", ""), None);
    }

    #[test]
    fn a_stale_reference_finds_its_file_by_name_wherever_it_landed() {
        let all = set(&[("concrete_hr_3.png", AncestorTextures), ("barn_hr_1.png", AncestorTextures)]);
        let ask = Ask { model: "bench", material: "", reference: Some(r"C:\Other\Files\concrete_hr_3.png"), linked_elsewhere: false };
        assert_eq!(all[ranked(&all, &ask)[0]].stem, "concrete_hr_3");
    }

    #[test]
    fn a_partly_textured_model_only_gains_a_clear_match() {
        let all = set(&[("Material_Base_color.png", Same), ("glass_tint.png", Same)]);
        let ask = |material| Ask { model: "lantern", material, reference: None, linked_elsewhere: true };
        assert!(ranked(&all, &ask("Material")).is_empty(), "a generic map could be for the part that already has it");
        assert_eq!(all[ranked(&all, &ask("glass"))[0]].stem, "glass_tint");
    }

    #[test]
    fn a_height_map_beside_a_base_colour_is_never_the_colour() {
        let all = set(&[("Material_Base_color.png", Same), ("Material_Height.png", Same)]);
        assert_eq!(all[1].role, Role::Data);
    }

    #[test]
    fn a_shortened_name_still_matches() {
        assert_eq!(best(&[("metalbat_default.png", AncestorTextures), ("woodenbat_default.png", AncestorTextures)], "metalbaseballbat", "").as_deref(), Some("metalbat_default"));
        assert!(!words_match("cleaver", "cleanser"));
    }

    #[test]
    fn a_grey_texture_is_kept_when_its_name_settles_it() {
        let all = set(&[("cobweb_1.png", AncestorTextures), ("base.png", Child), ("speckle.png", Same)]);
        let ask = Ask { model: "web", material: "cobweb_1", reference: Some("C:/cobweb_1.png"), linked_elsewhere: false };
        assert!(named_outright(&all[0], &ask));
        assert!(named_outright(&all[1], &ask), "`base` is a colour word");
        assert!(!named_outright(&all[2], &ask));
    }

    #[test]
    fn pixels_catch_what_names_miss() {
        let flat = |rgb: [u8; 3]| Image { width: 8, height: 8, rgba: [rgb[0], rgb[1], rgb[2], 255].repeat(64) };
        assert!(!looks_like_colour(&flat([128, 128, 255]), false), "a normal map");
        assert!(!looks_like_colour(&flat([128, 128, 255]), true), "even if it's named as colour");
        assert!(!looks_like_colour(&flat([90, 90, 90]), false), "a data map, by its perfect grey");
        assert!(looks_like_colour(&flat([90, 90, 90]), true), "unless it says it's colour");
        assert!(looks_like_colour(&flat([120, 80, 40]), false));
    }
}
