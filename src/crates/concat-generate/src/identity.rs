// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! A channel's look, the second stage of the channel manager (ROADMAP,
//! "Channel manager"): its colours, a tagline and the description its page
//! carries, the style every thumbnail keeps to, and its logo and banner,
//! drawn by the picture maker the person uses.
//!
//! The writer designs it from the channel as its owner describes it - the
//! niche picked, or the channel's fields as typed - and the two pictures
//! are drawn from the writer's prompts: the logo square, as YouTube crops
//! it to a circle, the banner wide, with what matters in its middle, where
//! every screen shows it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::makers::Pictures;

/// A channel's look, as the writer designs it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Identity {
    /// The main colour, `#rrggbb`: the videos' accent.
    pub colour: String,
    /// A second colour that goes with it, `#rrggbb`.
    pub second: String,
    /// A line under the channel's name, in its language.
    pub tagline: String,
    /// The channel page's description, in its language.
    pub about: String,
    /// What every thumbnail keeps to, as an image prompt in English.
    pub thumbnail_style: String,
    /// The logo, as an image prompt in English.
    pub logo_prompt: String,
    /// The banner, as an image prompt in English.
    pub banner_prompt: String,
}

/// The standing brief for designing a channel's look.
const SYSTEM: &str = "You are a brand designer for YouTube channels of narrated, faceless \
    videos. You design looks that read at a glance on a phone: few colours, strong contrast, \
    one clear idea per picture, nothing that dates fast. Image prompts you write are in \
    English, concrete and visual, and never ask for words drawn in the picture: a picture \
    model spells badly.";

/// The prompt for the look of the channel `channel` describes, its words
/// in `language`.
pub fn prompt(channel: &str, language: &str) -> String {
    format!(
        "The channel, as its owner describes it:\n{}\n\nLanguage of the channel: {language}\n\n\
         Design its look:\n\
         - colour: its main colour as #rrggbb, readable as text on dark pictures;\n\
         - second: a second colour that goes with it, #rrggbb;\n\
         - tagline: a line under its name, at most eight words, in {language};\n\
         - about: its page's description, three short paragraphs, in {language}: what it is, \
         what a viewer gets, how often it posts - no hashtags;\n\
         - thumbnail_style: the style every thumbnail keeps to, one sentence in English, as an \
         image prompt adds it (light, framing, colours, mood);\n\
         - logo_prompt: its logo as an image prompt in English: a simple symbol, centred, on a \
         plain background in its colours, that reads at 48 pixels and survives a circular \
         crop - no words, no letters;\n\
         - banner_prompt: its banner as an image prompt in English: wide, its subject in the \
         middle third, calm at the edges, in its colours - no words, no letters.",
        channel.trim()
    )
}

/// The tool the writer answers through.
pub fn tool() -> Value {
    let string = json!({"type": "string"});
    json!({
        "name": "identity",
        "description": "A channel's look.",
        "input_schema": {
            "type": "object",
            "properties": {
                "colour": string, "second": string, "tagline": string, "about": string,
                "thumbnail_style": string, "logo_prompt": string, "banner_prompt": string
            },
            "required": [
                "colour", "second", "tagline", "about", "thumbnail_style", "logo_prompt",
                "banner_prompt"
            ]
        }
    })
}

/// A colour as `#rrggbb`, lower case, or `None` when it is not one: the
/// writer's "#C9A227", "c9a227" and "#c9a" all are.
pub fn colour_of(text: &str) -> Option<String> {
    let hex = text.trim().trim_start_matches('#');
    let full = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => hex.to_owned(),
        _ => return None,
    };
    full.chars()
        .all(|c| c.is_ascii_hexdigit())
        .then(|| format!("#{}", full.to_ascii_lowercase()))
}

/// The look in the writer's answer: a colour that is not one falls back to
/// a gold that reads on dark pictures, and the second to the first.
pub fn of_answer(got: &Value) -> Identity {
    let text = |key: &str| got[key].as_str().unwrap_or_default().trim().to_owned();
    let colour = colour_of(&text("colour")).unwrap_or_else(|| "#c9a227".to_owned());
    let second = colour_of(&text("second")).unwrap_or_else(|| colour.clone());
    Identity {
        colour,
        second,
        tagline: text("tagline"),
        about: text("about"),
        thumbnail_style: text("thumbnail_style"),
        logo_prompt: text("logo_prompt"),
        banner_prompt: text("banner_prompt"),
    }
}

/// The look of the channel `channel` describes, its words in `language`,
/// asked of the writer (`key`, `model`).
pub fn design(channel: &str, language: &str, key: &str, model: &str) -> Result<Identity, String> {
    if key.trim().is_empty() {
        return Err(crate::writer::needs_key(model, "design the channel"));
    }
    if channel.trim().is_empty() {
        return Err("the channel has no name or description to design a look for".to_owned());
    }
    let prompt = prompt(channel, language);
    let got = crate::providers::retry(|| {
        crate::writer::call_as(key, model, SYSTEM, &[], &tool(), &prompt, &[])
    })?;
    Ok(of_answer(&got))
}

/// Where a channel's look is kept: a folder of its own under `data`, by
/// its name, so two channels never share a logo.
pub fn folder(data: &Path, channel: &str) -> PathBuf {
    let name: String = channel
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = match name.trim() {
        "" => "channel".to_owned(),
        name => name.to_owned(),
    };
    data.join("channels").join(name)
}

/// The look's logo and banner, drawn by `pictures` into `dir`, the logo
/// square and the banner wide; each that was drawn before is drawn again.
/// Their paths, logo first.
pub fn draw(
    identity: &Identity,
    pictures: &Pictures,
    dir: &Path,
) -> Result<(PathBuf, PathBuf), String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let logo =
        crate::providers::retry(|| pictures.make(&identity.logo_prompt, "1:1", dir, "logo"))?;
    let banner =
        crate::providers::retry(|| pictures.make(&identity.banner_prompt, "16:9", dir, "banner"))?;
    let kept = serde_json::to_vec_pretty(identity).map_err(|error| error.to_string())?;
    std::fs::write(dir.join("identity.json"), kept).map_err(|error| error.to_string())?;
    Ok((
        crate::providers::true_name(&logo),
        crate::providers::true_name(&banner),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Colours are read in the shapes a writer gives them, and nonsense is
    /// none.
    #[test]
    fn colours_are_read_or_refused() {
        assert_eq!(colour_of("#C9A227").as_deref(), Some("#c9a227"));
        assert_eq!(colour_of(" c9a227 ").as_deref(), Some("#c9a227"));
        assert_eq!(colour_of("#c9a").as_deref(), Some("#cc99aa"));
        assert_eq!(colour_of("gold"), None);
        assert_eq!(colour_of("#12345"), None);
    }

    /// The answer's look, a bad colour given a fallback and the second
    /// following the first.
    #[test]
    fn a_look_is_read_from_the_answer() {
        let got = json!({"colour": "blue", "second": "", "tagline": " Ideas ",
                         "logo_prompt": "an owl"});
        let look = of_answer(&got);
        assert_eq!(look.colour, "#c9a227");
        assert_eq!(look.second, "#c9a227");
        assert_eq!(look.tagline, "Ideas");
        assert_eq!(look.logo_prompt, "an owl");
    }

    /// Each channel's look has a folder of its own, its name made safe.
    #[test]
    fn each_channel_has_its_own_folder() {
        let data = Path::new("/data");
        assert_eq!(
            folder(data, "La Lógica"),
            Path::new("/data/channels/La Lógica")
        );
        assert_eq!(folder(data, "a/b"), Path::new("/data/channels/a_b"));
        assert_eq!(folder(data, "  "), Path::new("/data/channels/channel"));
    }
}
