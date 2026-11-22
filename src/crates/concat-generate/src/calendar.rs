// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! A channel's next thirty days, the third stage of the channel manager
//! (ROADMAP, "Channel manager"): the long videos to make, in order, each
//! on its day, with its angle and why a viewer clicks. Each long video
//! gives its four Shorts from the results (`shorts::PLANNED`), so the plan
//! is of long videos only.
//!
//! The writer is told the channel, what it made already - nothing is made
//! twice - and how many videos a week the person can afford; the cost of
//! the whole plan is shown before any of it is made. The plan is kept with
//! the channel, beside its look, and a video made is marked so: the fourth
//! stage makes them one after another from it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How many days a plan covers.
pub const DAYS: u32 = 30;

/// How many long videos a week a plan may hold, from the fewest.
pub const PER_WEEK: [u32; 4] = [1, 2, 3, 5];

/// One video of a plan.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// The day it goes up, 1 to [`DAYS`].
    pub day: u32,
    /// Its title, in the channel's language: the topic it is made from.
    pub title: String,
    /// Its angle - why a viewer clicks - in a sentence.
    pub angle: String,
    /// Made already, from the plan or by hand.
    pub made: bool,
}

/// A channel's plan.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Calendar {
    /// The videos, by day.
    pub entries: Vec<Entry>,
    /// Planned as machines to build ("What are we building?"), each
    /// titled by its name: a plan of the other kind does not fit that
    /// format, nor this one the others.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub machines: bool,
}

impl Calendar {
    /// How many videos a plan of `per_week` a week holds over [`DAYS`].
    pub fn count_for(per_week: u32) -> usize {
        (per_week.max(1) as usize * DAYS as usize).div_ceil(7)
    }

    /// The days `count` videos go up on, spread over [`DAYS`] as evenly as
    /// they go, the first on day 1.
    pub fn days_for(count: usize) -> Vec<u32> {
        if count == 0 {
            return Vec::new();
        }
        (0..count)
            .map(|n| 1 + (n as f64 * f64::from(DAYS) / count as f64).floor() as u32)
            .collect()
    }

    /// The first video not made yet.
    pub fn next(&self) -> Option<&Entry> {
        self.entries.iter().find(|entry| !entry.made)
    }

    /// Marks the video titled `title` made, when the plan holds it.
    pub fn mark_made(&mut self, title: &str) -> bool {
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.title.trim() == title.trim())
        {
            Some(entry) => {
                entry.made = true;
                true
            }
            None => false,
        }
    }

    /// Where a channel's plan is kept: in its folder (see
    /// [`crate::identity::folder`]).
    pub fn file(channel_dir: &Path) -> PathBuf {
        channel_dir.join("plan.json")
    }

    /// The plan kept in `channel_dir`, if one was.
    pub fn read(channel_dir: &Path) -> Option<Calendar> {
        let bytes = std::fs::read(Calendar::file(channel_dir)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Keeps the plan in `channel_dir`.
    pub fn keep(&self, channel_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(channel_dir).map_err(|error| error.to_string())?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        std::fs::write(Calendar::file(channel_dir), bytes).map_err(|error| error.to_string())
    }
}

/// The standing brief for planning a channel's month.
const SYSTEM: &str = "You plan the next month of a YouTube channel of narrated, faceless \
    videos. A good plan opens with the videos most likely to be found and watched to the end, \
    mixes the channel's subjects so no two videos in a row feel alike, and never repeats what \
    the channel made: YouTube does not pay for repetitive content, so every video says \
    something of its own.";

/// The prompt for `count` videos for the channel `channel` describes, in
/// `language`, none like `made`; with `machines`, each video is a machine
/// to build ("What are we building?"), named and nothing more.
pub fn prompt(
    channel: &str,
    language: &str,
    made: &[String],
    count: usize,
    machines: bool,
) -> String {
    let mut prompt = format!(
        "The channel, as its owner describes it:\n{}\nLanguage of the channel: {language}\n",
        channel.trim()
    );
    if !made.is_empty() {
        prompt += &format!("Already made - nothing like these: {}\n", made.join("; "));
    }
    prompt += &if machines {
        format!(
            "\nEvery video of this channel builds one machine or object in front of the viewer, \
             part by part, from something silly, and names it only at the end. Plan its next \
             {count}, in the order they go up, the first the one most people use without knowing \
             how it works. For each: as the title, only the machine's name, in {language} (a \
             submarine, an electric razor, a lift) - never a sentence; and, in one sentence, the \
             surprise in how it works, in {language}. No two in a row from the same field."
        )
    } else {
        format!(
            "\nPlan its next {count} long videos, in the order they go up, the first the most \
             likely to be found and watched to the end. Each a specific question, story or claim, \
             never a broad subject; no two in a row alike in subject or angle. For each: a title of \
             at most ten words and, in one sentence, why a viewer clicks - both in {language}."
        )
    };
    prompt
}

/// The tool the writer answers through.
pub fn tool() -> Value {
    json!({
        "name": "plan",
        "description": "A channel's next videos, in order.",
        "input_schema": {
            "type": "object",
            "properties": {
                "videos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "angle": {"type": "string"}
                        },
                        "required": ["title", "angle"]
                    }
                }
            },
            "required": ["videos"]
        }
    })
}

/// The plan in the writer's answer: at most `count` videos, each on its
/// day, one already made or named twice left out.
pub fn of_answer(got: &Value, made: &[String], count: usize) -> Calendar {
    let text = |value: &Value| value.as_str().unwrap_or_default().trim().to_owned();
    let mut seen: Vec<String> = made
        .iter()
        .map(|title| title.trim().to_lowercase())
        .collect();
    let mut videos: Vec<(String, String)> = Vec::new();
    for video in got["videos"].as_array().into_iter().flatten() {
        let title = text(&video["title"]);
        let key = title.to_lowercase();
        if title.is_empty() || seen.contains(&key) || videos.len() == count {
            continue;
        }
        seen.push(key);
        videos.push((title, text(&video["angle"])));
    }
    let days = Calendar::days_for(videos.len());
    Calendar {
        entries: videos
            .into_iter()
            .zip(days)
            .map(|((title, angle), day)| Entry {
                day,
                title,
                angle,
                made: false,
            })
            .collect(),
    }
}

/// A plan of `per_week` long videos a week over [`DAYS`] for the channel
/// `channel` describes, in `language`, none like `made`, asked of the
/// writer (`key`, `model`).
/// With `machines`, each video is a machine to build; see [`prompt`].
#[allow(clippy::too_many_arguments)]
pub fn plan(
    channel: &str,
    language: &str,
    made: &[String],
    per_week: u32,
    machines: bool,
    key: &str,
    model: &str,
) -> Result<Calendar, String> {
    if key.trim().is_empty() {
        return Err(crate::writer::needs_key(model, "plan the month"));
    }
    if channel.trim().is_empty() {
        return Err("the channel has no name or description to plan for".to_owned());
    }
    let count = Calendar::count_for(per_week);
    let prompt = prompt(channel, language, made, count, machines);
    let got = crate::providers::retry(|| {
        crate::writer::call_as(key, model, SYSTEM, &[], &tool(), &prompt, &[])
    })?;
    let mut calendar = of_answer(&got, made, count);
    calendar.machines = machines;
    if calendar.entries.is_empty() {
        return Err("the writer planned no videos".to_owned());
    }
    Ok(calendar)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A week's number becomes the month's, spread from day 1 over thirty.
    #[test]
    fn videos_are_spread_over_the_month() {
        assert_eq!(Calendar::count_for(3), 13);
        assert_eq!(Calendar::count_for(1), 5);
        assert_eq!(Calendar::days_for(3), [1, 11, 21]);
        let days = Calendar::days_for(13);
        assert_eq!(days.len(), 13);
        assert_eq!(days[0], 1);
        assert!(days.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(*days.last().unwrap() <= DAYS);
        assert!(Calendar::days_for(0).is_empty());
    }

    /// What was made already, and a title named twice, are left out; the
    /// rest are dated in order.
    #[test]
    fn the_plan_is_read_without_repeats() {
        let got = json!({"videos": [
            {"title": "Old one", "angle": "a"},
            {"title": "New one", "angle": "b"},
            {"title": "new ONE", "angle": "c"},
            {"title": "", "angle": "d"},
            {"title": "Third", "angle": "e"}
        ]});
        let plan = of_answer(&got, &["old one".to_owned()], 5);
        let titles: Vec<&str> = plan.entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["New one", "Third"]);
        assert_eq!(plan.entries[0].day, 1);
        assert_eq!(plan.entries[1].day, 16);
    }

    /// A plan of machines asks for their names alone; a plan of videos
    /// for titles.
    #[test]
    fn a_plan_of_machines_asks_for_names() {
        let machines = prompt("A channel", "English", &[], 5, true);
        assert!(machines.contains("only the machine's name"));
        let videos = prompt("A channel", "English", &[], 5, false);
        assert!(videos.contains("a title of at most ten words"));
        assert!(!videos.contains("machine's name"));
    }

    /// A video made is marked, the next is the first not made, and the
    /// plan is kept and read back.
    #[test]
    fn a_plan_is_kept_and_followed() {
        let mut plan = Calendar {
            entries: vec![
                Entry {
                    day: 1,
                    title: "One".into(),
                    ..Entry::default()
                },
                Entry {
                    day: 8,
                    title: "Two".into(),
                    ..Entry::default()
                },
            ],
            machines: true,
        };
        assert!(plan.mark_made(" One "));
        assert!(!plan.mark_made("Three"));
        assert_eq!(plan.next().map(|e| e.title.as_str()), Some("Two"));
        let dir = std::env::temp_dir().join(format!("concat-calendar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        plan.keep(&dir).unwrap();
        assert_eq!(Calendar::read(&dir), Some(plan));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
