// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! A niche for a channel that does not exist yet: the first stage of the
//! channel manager (ROADMAP, "Channel manager").
//!
//! The person says what they care about and in what language they would
//! speak; the writer proposes niches, each judged on what decides whether
//! a channel made of narrated videos can grow - how many look for it, how
//! crowded it is, what its views pay, and whether it can be made without a
//! face on camera - and each with the channel it would be: who watches,
//! how it sounds, what it covers, the rules every script keeps to, and its
//! first videos. One picked becomes a channel the Long video sheet writes
//! for, as the channel the app comes with is.
//!
//! The figures are the writer's judgment, out of ten, not data looked up:
//! the sheet says so. Nothing here is paid for but the writer's answer.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How many niches are proposed at once.
pub const PROPOSED: usize = 6;

/// One niche, as the writer proposes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Niche {
    /// The channel's name: short, in the channel's language.
    pub name: String,
    /// The niche in a line: what the channel is about.
    pub summary: String,
    /// Why it can grow, in a sentence or two.
    pub why: String,
    /// How many look for it, 0 to 10.
    pub demand: u8,
    /// How crowded it is, 0 (empty) to 10 (crowded).
    pub competition: u8,
    /// What its views tend to pay in advertising, 0 to 10.
    pub revenue: u8,
    /// How well it is made as narrated video without a face, 0 to 10.
    pub faceless: u8,
    /// Who watches it.
    pub audience: String,
    /// How the channel sounds.
    pub voice: String,
    /// What it covers: the subjects its ideas come from.
    pub topics: String,
    /// The editorial rules every script keeps to.
    pub rules: String,
    /// Its first videos' titles.
    pub first_videos: Vec<String>,
}

impl Niche {
    /// The niche's worth, 0 to 10, for ordering them: demand, what it pays
    /// and how well it suits faceless video count for it, a crowd against
    /// it. A guide, not a verdict.
    pub fn score(&self) -> f32 {
        let room = 10 - self.competition.min(10);
        (f32::from(self.demand) * 3.0
            + f32::from(self.revenue) * 2.0
            + f32::from(self.faceless) * 2.0
            + f32::from(room) * 3.0)
            / 10.0
    }

    /// The channel as the writer is told it: the same words the channel's
    /// fields hold once it is picked.
    pub fn channel_text(&self) -> String {
        [
            ("Channel", &self.name),
            ("About", &self.summary),
            ("Who watches", &self.audience),
            ("How it sounds", &self.voice),
            ("What it covers", &self.topics),
            ("Rules", &self.rules),
        ]
        .iter()
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|(label, value)| format!("{label}: {}", value.trim()))
        .collect::<Vec<_>>()
        .join("\n")
    }
}

/// The standing brief for proposing niches.
const SYSTEM: &str = "You advise people starting a YouTube channel of narrated, faceless \
    videos - a voice over pictures, animated cards and clips, made with AI. You know what \
    makes such a channel grow and what gets it refused for monetisation: YouTube does not pay \
    for mass-produced, repetitive or reused content, so every niche you propose must give room \
    for videos that each say something of their own, researched, with a point of view. You are \
    honest: a crowded niche is said to be crowded, a niche that pays little is said to, and \
    your figures are judgments, never invented statistics.";

/// The prompt for `count` niches for someone who cares about `interests`
/// and would speak `language`.
pub fn prompt(interests: &str, language: &str, count: usize) -> String {
    let interests = match interests.trim() {
        "" => "nothing in particular: propose what suits a new faceless channel best",
        interests => interests,
    };
    format!(
        "Language the channel speaks: {language}\nWhat its owner cares about: {interests}\n\n\
         Propose {count} niches for a new channel of narrated, faceless videos, each different \
         from the others, each specific enough to own (\"history\" is a field; \"the economics \
         of failed empires\" is a niche). For each give:\n\
         - name: the channel's name, short and memorable, in {language};\n\
         - summary: the niche in one line;\n\
         - why: in one or two sentences, why it can grow now;\n\
         - demand, competition, revenue, faceless: your judgment out of 10 - how many look for \
         it, how crowded it is (10 crowded), what its views pay in advertising, how well it is \
         made without a face on camera;\n\
         - audience: who watches it, in a sentence;\n\
         - voice: how the channel sounds - tone, pace, the narrator;\n\
         - topics: the subjects its videos come from, separated by semicolons;\n\
         - rules: three or four editorial rules every script keeps to, for videos that each say \
         something of their own;\n\
         - first_videos: five titles for its first videos, each a specific question, story or \
         claim, never a broad subject.\n\
         Write every field but the scores in {language}."
    )
}

/// The tool the writer answers through.
pub fn tool() -> Value {
    let string = json!({"type": "string"});
    let score = json!({"type": "integer", "minimum": 0, "maximum": 10});
    json!({
        "name": "niches",
        "description": "Niches for a new faceless channel.",
        "input_schema": {
            "type": "object",
            "properties": {
                "niches": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": string, "summary": string, "why": string,
                            "demand": score, "competition": score,
                            "revenue": score, "faceless": score,
                            "audience": string, "voice": string,
                            "topics": string, "rules": string,
                            "first_videos": {"type": "array", "items": string}
                        },
                        "required": [
                            "name", "summary", "why", "demand", "competition", "revenue",
                            "faceless", "audience", "voice", "topics", "rules", "first_videos"
                        ]
                    }
                }
            },
            "required": ["niches"]
        }
    })
}

/// The niches in the writer's answer, the most worth first: a niche with
/// no name is dropped, scores are held to 0..=10, and two of one name are
/// one.
pub fn of_answer(got: &Value, count: usize) -> Vec<Niche> {
    let text = |value: &Value| value.as_str().unwrap_or_default().trim().to_owned();
    let score = |value: &Value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
            .map_or(0, |n| n.round().clamp(0.0, 10.0) as u8)
    };
    let mut niches: Vec<Niche> = Vec::new();
    for niche in got["niches"].as_array().into_iter().flatten() {
        let name = text(&niche["name"]);
        if name.is_empty() || niches.iter().any(|n| n.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        niches.push(Niche {
            name,
            summary: text(&niche["summary"]),
            why: text(&niche["why"]),
            demand: score(&niche["demand"]),
            competition: score(&niche["competition"]),
            revenue: score(&niche["revenue"]),
            faceless: score(&niche["faceless"]),
            audience: text(&niche["audience"]),
            voice: text(&niche["voice"]),
            topics: text(&niche["topics"]),
            rules: text(&niche["rules"]),
            first_videos: niche["first_videos"]
                .as_array()
                .into_iter()
                .flatten()
                .map(text)
                .filter(|title| !title.is_empty())
                .collect(),
        });
    }
    niches.sort_by(|a, b| b.score().total_cmp(&a.score()));
    niches.truncate(count);
    niches
}

/// [`PROPOSED`] niches for someone who cares about `interests` and would
/// speak `language`, asked of the writer (`key`, `model`), the most worth
/// first.
pub fn find(interests: &str, language: &str, key: &str, model: &str) -> Result<Vec<Niche>, String> {
    if key.trim().is_empty() {
        return Err(crate::writer::needs_key(model, "suggest niches"));
    }
    let prompt = prompt(interests, language, PROPOSED);
    let got = crate::providers::retry(|| {
        crate::writer::call_as(key, model, SYSTEM, &[], &tool(), &prompt, &[])
    })?;
    let niches = of_answer(&got, PROPOSED);
    if niches.is_empty() {
        return Err("the writer proposed no niches".to_owned());
    }
    Ok(niches)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer is read whatever shape its scores come in, held to ten,
    /// the nameless and the repeated dropped, the most worth first.
    #[test]
    fn niches_are_read_and_ranked() {
        let got = json!({"niches": [
            {"name": "Crowded", "demand": 9, "competition": 10, "revenue": 5, "faceless": 8},
            {"name": "Open", "demand": "8", "competition": 2.4, "revenue": 7, "faceless": 15,
             "first_videos": ["One", " ", "Two"], "topics": "a; b"},
            {"name": "", "demand": 10},
            {"name": "open", "demand": 1}
        ]});
        let niches = of_answer(&got, 6);
        let names: Vec<&str> = niches.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Open", "Crowded"]);
        assert_eq!(niches[0].demand, 8);
        assert_eq!(niches[0].competition, 2);
        assert_eq!(niches[0].faceless, 10);
        assert_eq!(niches[0].first_videos, ["One", "Two"]);
        assert_eq!(of_answer(&got, 1).len(), 1);
        assert!(of_answer(&json!({}), 6).is_empty());
    }

    /// The channel's text holds what was filled in, and nothing for what
    /// was not.
    #[test]
    fn a_niche_tells_the_writer_its_channel() {
        let niche = Niche {
            name: "La Lógica".into(),
            audience: "Adults".into(),
            ..Niche::default()
        };
        assert_eq!(
            niche.channel_text(),
            "Channel: La Lógica\nWho watches: Adults"
        );
    }

    /// The prompt asks for the number and the language, and for anything
    /// when nothing is cared about in particular.
    #[test]
    fn the_prompt_names_the_language_and_the_count() {
        let asked = prompt("", "العربية", 6);
        assert!(asked.contains("Propose 6 niches"));
        assert!(asked.contains("in العربية"));
        assert!(asked.contains("nothing in particular"));
    }
}
