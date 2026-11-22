// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! The Long video sheet: an idea in, a cut timeline out.
//!
//! Three pieces of work, two on a worker and one here. Writing the script
//! asks Claude for the storyboard; making asks the chosen providers for the
//! narration and the shots, and probes what they wrote. Both only read and
//! write the job's folder inside the project folder, so a run stopped half
//! way carries on where it was, and a storyboard edited by hand is the one
//! used. The cut is laid on the window's session, as one undo step, on a
//! timeline of its own: Undo takes it all away, and the timeline the person
//! was cutting is not touched.
//!
//! See `concat_generate` for the steps themselves.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use concat_generate::budget::{self, Totals};
use concat_generate::calendar;
use concat_generate::checklist;
use concat_generate::cost::{self, Estimate, Prices};
use concat_generate::identity;
use concat_generate::job::{Costs, Idea, Job, Made, Remade, Status, Step};
use concat_generate::makers;
use concat_generate::niche;
use concat_generate::plan::{self, CaptionLook, Editing, Options};
use concat_generate::providers::{self, DEFAULT_VIDEO_MODEL, Shots, Voice};
use concat_generate::review;
use concat_generate::shorts;
use concat_generate::storyboard::{Scene, ShotKind, Storyboard, shot_stem};
use concat_generate::writer::{
    self, Brief, ContentStyle, Level, PictureStyle, Publishing, Research, Suggestion, VideoFormat,
    VideoIdea, Writing,
};
use concat_generate::youtube;
use concat_project::Command;
use concat_project::commands::NewMedia;
use concat_project::model::MediaKind;
use slint::SharedString;

use crate::host::{image_at, on_ui, on_ui_in_project, spawn, spawn_in_project};
use crate::i18n::{t, tf};
use crate::panes::Msg;
use crate::panes::export::ExportMsg;
use crate::panes::settings::installed;
use crate::studio::Studio;
use crate::ui::{GenerateSheetData, NicheRow, PlanRow, ScriptRow, ScriptSheetData, ShortResult};

/// The lengths offered, in minutes, row for row with the sheet's list.
const MINUTES: [f64; 8] = [1.0, 3.0, 5.0, 10.0, 15.0, 20.0, 30.0, 60.0];
/// The row a first opening starts on: ten minutes.
const DEFAULT_MINUTES: usize = 3;
/// The languages offered, by the English name the writer is told, row for
/// row with the sheet's list.
const LANGUAGES: [&str; 10] = [
    "Arabic",
    "English",
    "French",
    "Spanish",
    "German",
    "Turkish",
    "Portuguese",
    "Indonesian",
    "Hindi",
    "Urdu",
];
/// The frames offered.
const ASPECTS: [&str; 3] = ["16:9", "9:16", "1:1"];
/// The shares of shots made as clips, row for row with the sheet's list.
const CLIP_SHARES: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
/// The row a first opening starts on: a quarter of the shots clips, the
/// rest stills - clips cost several times what a still does.
const DEFAULT_CLIPS: usize = 1;
/// The Kokoro speaker a read starts with when none has been chosen.
const DEFAULT_SPEAKER: i32 = 3;
/// The input a Replicate model is sent when nothing else is known about it.
const REPLICATE_INPUT: &str = r#"{"prompt": "{prompt}", "aspect_ratio": "{aspect}"}"#;

/// Everything that can happen to the Long video sheet.
#[derive(Debug)]
pub enum GenerateMsg {
    /// The menu's Long video.
    Open,
    Close,
    /// See the sheet's `text-edited` for the fields.
    TextEdited(i32, String),
    /// See the sheet's `choice-changed` for the fields.
    ChoiceChanged(i32, i32),
    /// See the sheet's `toggled` for the fields.
    Toggled(i32, bool),
    /// Write the storyboard, and stop there.
    WriteScript,
    /// Make whatever is missing, the script included, and cut it.
    Create,
    /// Show the job's folder in the file manager.
    OpenFolder,
    /// Open the script editor on the storyboard.
    EditScript,
    /// Close it, writing the storyboard back.
    CloseScript,
    /// A text of the script changed: chapter, scene and shot (-1 where the
    /// row is none) and which text; see the editor's `edited`.
    ScriptEdited(i32, i32, i32, i32, String),
    /// A shot made a clip, or a still.
    ShotKindChanged(i32, i32, i32, bool),
    /// Write one scene again.
    RewriteScene(i32, i32),
    /// The writer is done with a scene.
    Rewritten(usize, usize, Box<Result<Scene, String>>),
    Cancel,
    /// A worker saying where it is: `0..=1`, and in words.
    Progress(f32, String, String),
    /// The making saying where it is.
    Step(Step),
    /// Make one shot of the script again: chapter, scene and shot.
    Remake(i32, i32, i32),
    /// Make the shot the selected clip shows again.
    RemakeClip,
    /// Ask the writer for another picture for one shot: chapter, scene
    /// and shot.
    Suggest(i32, i32, i32),
    /// The writer's other picture, or why there is none.
    Suggested(usize, usize, usize, Box<Result<Suggestion, String>>),
    /// An explainer designed again: chapter, scene, shot, and the design.
    Designed(
        usize,
        usize,
        usize,
        Box<Result<concat_generate::explainer::Explainer, String>>,
    ),
    /// A shot is made again, or why not: its stem and its job's folder.
    Remade(String, PathBuf, Box<Result<(Remade, NewMedia), String>>),
    /// Small pictures of shots, by stem: width, height and RGBA.
    /// Small pictures of shots made, by stem, each with its likeness
    /// when it has detail enough to compare; and whether it is the last
    /// of them.
    Thumbs(Vec<Thumb>, bool),
    /// The picture models the account can run, or why there is no list:
    /// asked of the maker (1 OpenAI, 2 Gemini) with that key.
    /// The key is told by its fingerprint, not itself: every message is
    /// written to the log (it was, key and all - the owner's log, 21 Nov).
    PictureModels(usize, u64, Box<Result<Vec<String>, String>>),
    /// Make Short `n` of the results into a project of its own.
    MakeShort(i32),
    /// Short `n`'s storyboard and the files it is cut from, or why not.
    ShortMade(usize, Box<Result<(Storyboard, Made), String>>),
    /// Open the project Short `n` was made into.
    OpenShort(i32),
    /// Back on the long video, the sheet closed.
    OpenVideo,
    /// One of the sheet's buttons: 0 suggest ideas, 1 make every idea,
    /// 2 make the video again in another language, 3 save the choices as
    /// a series, 4 pick the video's Shorts, 5 research first, to read and
    /// correct the notes before the script is written from them, 6 export
    /// every video of the project - the long one and its Shorts - into the
    /// video's folder, one after another.
    Act(i32),
    /// The writer's ideas, or why there are none.
    Ideas(Box<Result<Vec<VideoIdea>, String>>),
    /// The research done on its own, or why not.
    Researched(Box<Result<Research, String>>),
    /// The next video of Export everything is due, once the one before is
    /// written.
    ExportNext,
    /// The next idea of a batch is due, once nothing is running.
    Next,
    /// The video translated: its new idea, its storyboard and its
    /// language's row, or why not.
    Translated(Box<Result<(String, Storyboard, usize), String>>),
    /// The channel's look designed, its logo and banner drawn when a
    /// picture maker is chosen (else why not), or why there is none.
    Looked(Box<Result<Look, String>>),
    /// Signed in to YouTube: the refresh token and the channel's title,
    /// or why not.
    YouTubeConnected(Box<Result<(String, String), String>>),
    /// A video put up: its id and title, or why not.
    Uploaded(Box<Result<(String, String), String>>),
    /// How far an upload is, 0 to 1.
    UploadProgress(f32),
    /// The numbers of the videos put up, or why there are none.
    YouTubeNumbers(Box<Result<Vec<youtube::Numbers>, String>>),
    /// The channel's month planned, or why not.
    Planned(Box<Result<calendar::Calendar, String>>),
    /// Make video `n` of the channel's plan next: its title the idea.
    UsePlanned(i32),
    /// Use niche `n` of those proposed: a channel of its own, picked.
    ChooseNiche(i32),
    /// The niches proposed, or why there are none.
    Niches(Box<Result<Vec<niche::Niche>, String>>),
    /// The Shorts picked for the results, or why not.
    ShortsPlanned(Box<Result<shorts::Plan, String>>),
    /// The writer is done; and whether making follows.
    Written(Box<Result<Storyboard, String>>, bool),
    /// The providers are done, and the files probed.
    Made(Box<Result<(Storyboard, Made), String>>),
    /// Write what goes up with the video on YouTube, and its thumbnail.
    Publish,
    /// A run somewhere moved on: drawn again, for the pill.
    Tick,
    /// The run on a job's folder is over: its folder, its title, and
    /// whether it made everything.
    Over(PathBuf, String, bool),
    /// The floating pill was pressed.
    Pill,
    /// What goes up with it, kept; the thumbnail's picture, probed, or why
    /// there is none; or why nothing was written.
    Published(Box<Result<Published, String>>),
}

/// The Long video sheet's state.
pub struct GeneratePane {
    pub open: bool,
    pub topic: String,
    pub notes: String,
    pub minutes: usize,
    pub captions: bool,
    pub titles: bool,
    /// Row of [`GeneratePane::music_rows`]; 0 is none, 1 music made for
    /// the video ([`AI_MUSIC`]).
    pub music: usize,
    pub running: bool,
    pub progress: f32,
    pub stage: String,
    pub message: String,
    /// Which step a run is on: 0 the script, 1 the narration, 2 the shots,
    /// 3 the cut.
    phase: i32,
    /// The run reads its scenes aloud.
    voiced: bool,
    /// The shots that failed have been made again once this run: a second
    /// failure is said, not tried a third time.
    mended: bool,
    /// What the video is made from, when it is made from something: a
    /// page's address or a file's path.
    source: String,
    /// The writer's ideas for the next videos.
    ideas: Vec<VideoIdea>,
    /// Finding a niche for a new channel: shown, what the person cares
    /// about, and the niches proposed, the most worth first.
    niche_open: bool,
    /// The channel's logo and banner as the sheet shows them: loaded once
    /// as the sheet opens or the look is drawn, not at every redraw.
    look_pictures: (slint::Image, slint::Image),
    /// The channel's plan, as kept with it.
    calendar: Option<calendar::Calendar>,
    /// YouTube: an upload under way and how far, and the last numbers
    /// read, in words.
    uploading: Option<f32>,
    youtube_numbers: String,
    niche_interests: String,
    niches: Vec<niche::Niche>,
    /// The ideas still to make, in a batch; the one being made is not
    /// among them.
    queue: Vec<String>,
    /// When the last run ended: the next of a batch waits a moment after
    /// it, for its export to start.
    ended: Option<std::time::Instant>,
    /// Row of the language the video is made again in.
    also_language: usize,
    /// The video's Shorts, each a project of its own once made.
    plan: Option<shorts::Plan>,
    /// The Short being made, of [`Self::plan`].
    making_short: Option<usize>,
    /// What the step under way is making, in the script's words.
    detail: String,
    /// The last shot made, to show while the next is.
    preview: Option<slint::Image>,
    /// Shots made of how many, as the steps show it.
    counts: String,
    /// What the run has paid so far, in dollars.
    spent: f64,
    /// How far the job is, said under the script's summary.
    status: String,
    /// Every shot the script asks for is made: a Short can be cut.
    all_made: bool,
    /// A small picture of each shot made, by stem, for the script editor.
    thumbs: HashMap<String, slint::Image>,
    /// The last picture check, by stem, as kept with the job: shown at
    /// each shot in the script editor; a shot made again since drops out.
    picture_checks: HashMap<String, concat_generate::visual::Check>,
    /// The shots being made again, by stem.
    remaking: HashSet<String>,
    /// What goes up with the video on YouTube, once written.
    publishing: Option<Publishing>,
    /// The writer is writing it.
    publishing_busy: bool,
    /// What each shot made looks like, roughly, by stem: to tell a picture
    /// too like the one before it.
    likeness: HashMap<String, Likeness>,
    /// The shots whose pictures came out too like the shot's before them,
    /// by stem: made again, they come out otherwise.
    lookalikes: HashSet<String>,
    /// The shots the writer is finding another picture for: chapter,
    /// scene and shot.
    suggesting: HashSet<(usize, usize, usize)>,
    /// The storyboard already in the job's folder, read when the sheet
    /// opens and whenever the idea changes.
    board: Option<Storyboard>,
    /// The shots whose pictures read as ones already used, by stem:
    /// worked out when the storyboard changes, not on every publish.
    repeats: HashSet<String>,
    /// The research done for the idea, as kept with the video: read and
    /// corrected in the sheet, and the script written from it.
    research: Option<Research>,
    /// Export everything: what is still to write, the timeline that was
    /// open before, the video's project, to go back to at the end, and the
    /// folder every file is written into - the video's, whichever project
    /// is open while a Short of it is written.
    exports: Vec<Exporting>,
    export_back: Option<String>,
    export_home: Option<String>,
    export_dir: Option<PathBuf>,
    /// Export everything has started a file of its own.
    export_started: bool,
    /// What the last check before publishing found, in the interface's
    /// language; None until it has run.
    checked: Option<Vec<String>>,
    /// The script editor is showing.
    pub script_open: bool,
    /// The scene being written again, if one is.
    rewriting: Option<(usize, usize)>,
    /// Why the last rewrite failed.
    script_message: String,
    /// The storyboard has edits the file does not have yet.
    dirty: bool,
    /// What the edits made stale: scenes whose narration changed and shots
    /// whose picture did, by id and stem, removed from the job's folder
    /// when the storyboard is written so the next run makes them again.
    stale_voices: BTreeSet<String>,
    stale_shots: BTreeSet<String>,
    /// The picture models offered for the chosen maker; see
    /// [`PictureModels`].
    picture_models: PictureModels,
    /// Asked between files by the running worker.
    stop: Arc<AtomicBool>,
    /// The phone is told to keep the app going: something is being made.
    kept_alive: bool,
}

impl Default for GeneratePane {
    fn default() -> Self {
        GeneratePane {
            open: false,
            topic: String::new(),
            notes: String::new(),
            minutes: DEFAULT_MINUTES,
            captions: true,
            titles: true,
            music: 0,
            running: false,
            progress: 0.0,
            stage: String::new(),
            message: String::new(),
            phase: 0,
            voiced: false,
            mended: false,
            source: String::new(),
            ideas: Vec::new(),
            niche_open: false,
            look_pictures: Default::default(),
            calendar: None,
            uploading: None,
            youtube_numbers: String::new(),
            niche_interests: String::new(),
            niches: Vec::new(),
            queue: Vec::new(),
            ended: None,
            also_language: 1,
            plan: None,
            making_short: None,
            detail: String::new(),
            preview: None,
            counts: String::new(),
            spent: 0.0,
            status: String::new(),
            all_made: false,
            thumbs: HashMap::new(),
            picture_checks: HashMap::new(),
            likeness: HashMap::new(),
            publishing: None,
            research: None,
            exports: Vec::new(),
            export_back: None,
            export_home: None,
            export_dir: None,
            export_started: false,
            checked: None,
            publishing_busy: false,
            lookalikes: HashSet::new(),
            remaking: HashSet::new(),
            suggesting: HashSet::new(),
            board: None,
            repeats: HashSet::new(),
            script_open: false,
            rewriting: None,
            script_message: String::new(),
            dirty: false,
            stale_voices: BTreeSet::new(),
            stale_shots: BTreeSet::new(),
            picture_models: PictureModels::default(),
            stop: Arc::new(AtomicBool::new(false)),
            kept_alive: false,
        }
    }
}

impl GeneratePane {
    /// Applies one message. The studio is the rest of the window; while
    /// this runs the studio's copy of the pane is a blank it must not read.
    pub fn update(&mut self, msg: GenerateMsg, studio: &mut Studio) {
        self.apply(msg, studio);
        self.hold();
    }

    /// A phone keeps the app going in the background while a run or a
    /// shot made again is under way, and lets go when it ends - told only
    /// when that changes. Nothing on a desktop.
    fn hold(&mut self) {
        let busy = self.running || !self.remaking.is_empty();
        if busy != self.kept_alive {
            self.kept_alive = busy;
            crate::platform::keep_alive(busy.then(|| t("generate.keptAlive")));
        }
    }

    /// What a message does, before the phone is told.
    fn apply(&mut self, msg: GenerateMsg, studio: &mut Studio) {
        match msg {
            GenerateMsg::Open => {
                self.open = true;
                self.message.clear();
                if let Some(minutes) = studio.prefs.generate.minutes {
                    self.minutes = minutes.min(MINUTES.len() - 1);
                }
                // Opened with no idea typed - the app started again, or the
                // project opened again - the sheet picks up the job worked
                // on last, with everything it has made so far.
                if self.topic.trim().is_empty()
                    && !self.running
                    && let Some(project) = studio.session.as_ref().map(|s| s.path().to_owned())
                    && let Some(idea) = Job::latest(Path::new(&project))
                {
                    self.topic = idea.topic;
                    self.notes = idea.notes;
                }
                self.ask_picture_models(studio);
                self.load_look_pictures(studio);
                self.look(studio);
            }
            GenerateMsg::Close => {
                self.keep(studio);
                self.open = false;
            }
            GenerateMsg::TextEdited(field, text) => {
                let prefs = &mut studio.prefs.generate;
                match field {
                    0 => {
                        self.topic = text;
                        self.look(studio);
                        return;
                    }
                    1 => {
                        self.notes = text;
                        return;
                    }
                    2 => prefs.anthropic_key = text,
                    3 => prefs.replicate_token = text,
                    4 => prefs.replicate_model = text,
                    5 => prefs.elevenlabs_key = text,
                    6 => prefs.elevenlabs_voice = text,
                    7 => prefs.replicate_image_model = text,
                    8 => prefs.spend_limit = text,
                    9 => prefs.clip_price = text,
                    10 => prefs.image_price = text,
                    11 => prefs.channel_name = text,
                    12 => prefs.channel_audience = text,
                    13 => prefs.channel_voice = text,
                    14 => prefs.channel_opening = text,
                    15 => prefs.channel_closing = text,
                    16 => prefs.channel_example = text,
                    17 => prefs.pronunciation = text,
                    // The chosen picture maker's key and model, and the
                    // chosen clip maker's: one pair of fields in the sheet
                    // for whichever is picked.
                    21 => match prefs.picture_maker {
                        1 => prefs.openai_key = text,
                        _ => prefs.gemini_key = text,
                    },
                    22 => match prefs.picture_maker {
                        1 => prefs.openai_image_model = text,
                        _ => prefs.gemini_image_model = text,
                    },
                    23 => match prefs.clip_maker {
                        1 => prefs.kling_access_key = text,
                        _ => prefs.seedance_key = text,
                    },
                    24 => prefs.kling_secret_key = text,
                    25 => match prefs.clip_maker {
                        1 => prefs.kling_model = text,
                        _ => prefs.seedance_model = text,
                    },
                    18 => prefs.brand_color = text,
                    20 => prefs.structure = text,
                    26 => prefs.channel_topics = text,
                    27 => prefs.channel_rules = text,
                    29 => prefs.openai_key = text,
                    30 => prefs.writer_model = text,
                    28 => {
                        // The notes as the person corrects them, kept with
                        // the video as they go; emptied, they are gone.
                        let job = self.job(studio);
                        let research = self.research.get_or_insert_with(Research::default);
                        research.notes = text;
                        // Theirs hold: the claims graded before are not
                        // said again over what they corrected.
                        research.claims.clear();
                        research.edited = true;
                        if let Some(job) = job
                            && let Err(error) = job.keep_research(research)
                        {
                            log::warn!("generate: the research notes: {error}");
                        }
                        if research.notes.trim().is_empty() {
                            self.research = None;
                        }
                        return;
                    }
                    19 => {
                        self.source = text;
                        return;
                    }
                    31 => {
                        self.niche_interests = text;
                        return;
                    }
                    32 => prefs.youtube_client_id = text,
                    33 => prefs.youtube_client_secret = text,
                    _ => return,
                }
                studio.prefs.save(&studio.host.dirs);
                // A key typed - the picture maker's, or OpenAI's from the
                // writer's row - is a list of models to ask for again.
                if matches!(field, 21 | 29) {
                    self.ask_picture_models(studio);
                }
            }
            GenerateMsg::ChoiceChanged(field, index) => {
                let index = index.max(0) as usize;
                let prefs = &mut studio.prefs.generate;
                match field {
                    0 => {
                        self.minutes = index.min(MINUTES.len() - 1);
                        prefs.minutes = Some(self.minutes);
                    }
                    1 => prefs.language = index.min(LANGUAGES.len() - 1),
                    2 => prefs.aspect = index.min(ASPECTS.len() - 1),
                    3 => {
                        prefs.voice = Some(index.min(2));
                        studio.prefs.save(&studio.host.dirs);
                        self.tally(studio);
                        return;
                    }
                    4 => prefs.shots = index.min(2),
                    5 => {
                        self.music = index;
                        return;
                    }
                    6 => prefs.clips = Some(index.min(CLIP_SHARES.len() - 1)),
                    7 => prefs.caption_look = index.min(1),
                    8 => prefs.content_style = index.min(ContentStyle::ALL.len() - 1),
                    12 => prefs.pictures = index.min(PictureStyle::ALL.len() - 1),
                    13 => prefs.elevenlabs_model = index.min(1),
                    19 => {
                        // A format sets what goes with it; the choices stay
                        // the person's to change after.
                        let format = index.min(VideoFormat::ALL.len() - 1);
                        prefs.format = format;
                        if let Some(look) = format_look(VideoFormat::ALL[format]) {
                            self.minutes = look.minutes;
                            prefs.minutes = Some(look.minutes);
                            prefs.aspect = look.aspect;
                            prefs.content_style = look.content_style;
                            prefs.level = Some(look.level);
                            prefs.pictures = look.pictures;
                            prefs.caption_look = look.caption_look;
                            prefs.clips = Some(look.clips);
                            prefs.visuals = 0;
                        }
                        studio.prefs.save(&studio.host.dirs);
                        self.tally(studio);
                        return;
                    }
                    16 => {
                        // The idea picked becomes the video's.
                        if let Some(idea) = index.checked_sub(1).and_then(|row| self.ideas.get(row))
                        {
                            self.topic = idea.title.clone();
                            self.look(studio);
                        }
                        return;
                    }
                    17 => {
                        self.also_language = index.min(LANGUAGES.len() - 1);
                        return;
                    }
                    18 => {
                        // The row after the series saved is the ready
                        // channel, while none is saved under its name:
                        // picked, it is saved, and brought back as one.
                        let prefs = &mut studio.prefs.generate;
                        if index.checked_sub(1) == Some(prefs.templates.len())
                            && !has_la_logica(prefs)
                        {
                            let ready = prefs.la_logica();
                            prefs.keep_template(ready);
                        }
                        if let Some(row) = index.checked_sub(1)
                            && studio.prefs.generate.apply_template(row)
                        {
                            studio.prefs.save(&studio.host.dirs);
                            self.load_look_pictures(studio);
                            self.tally(studio);
                        }
                        return;
                    }
                    14 => {
                        let picked = index
                            .checked_sub(1)
                            .and_then(|row| logo_items(studio).into_iter().nth(row))
                            .unwrap_or_default();
                        studio.prefs.generate.brand_logo = picked;
                        studio.prefs.save(&studio.host.dirs);
                        return;
                    }
                    10 => prefs.level = Some(index.min(Level::ALL.len() - 1)),
                    22 => prefs.shorts_count = Some(index.min(4) + 1),
                    25 => prefs.plan_per_week = Some(index.min(calendar::PER_WEEK.len() - 1)),
                    26 => prefs.youtube_privacy = index.min(2),
                    23 => prefs.writer = index.min(1),
                    20 => {
                        prefs.picture_maker = index.min(2);
                        studio.prefs.save(&studio.host.dirs);
                        self.ask_picture_models(studio);
                        self.tally(studio);
                        return;
                    }
                    24 => {
                        if let Some(name) = self.picture_models.names.get(index) {
                            match prefs.picture_maker {
                                1 => prefs.openai_image_model = name.clone(),
                                _ => prefs.gemini_image_model = name.clone(),
                            }
                        }
                    }
                    21 => {
                        prefs.clip_maker = index.min(2);
                        studio.prefs.save(&studio.host.dirs);
                        self.tally(studio);
                        return;
                    }
                    11 => {
                        prefs.visuals = index.min(1);
                        studio.prefs.save(&studio.host.dirs);
                        self.tally(studio);
                        return;
                    }
                    9 => {
                        let (model, price) = match index {
                            1 => (PROFESSIONAL_IMAGE_MODEL, PROFESSIONAL_IMAGE_PRICE),
                            2 => (BEST_IMAGE_MODEL, BEST_IMAGE_PRICE),
                            _ => ("", ""),
                        };
                        prefs.replicate_image_model = model.to_owned();
                        prefs.image_price = price.to_owned();
                        studio.prefs.save(&studio.host.dirs);
                        self.tally(studio);
                        return;
                    }
                    _ => return,
                }
                studio.prefs.save(&studio.host.dirs);
            }
            GenerateMsg::Toggled(field, on) => match field {
                0 => self.captions = on,
                1 => self.titles = on,
                2 => {
                    studio.prefs.generate.auto_export = Some(on);
                    studio.prefs.save(&studio.host.dirs);
                }
                3 => {
                    studio.prefs.generate.research = Some(on);
                    studio.prefs.save(&studio.host.dirs);
                }
                4 => {
                    studio.prefs.generate.sounds = Some(on);
                    studio.prefs.save(&studio.host.dirs);
                }
                5 => {
                    studio.prefs.generate.brand_bumpers = on;
                    studio.prefs.save(&studio.host.dirs);
                }
                6 => {
                    studio.prefs.generate.hook = Some(on);
                    studio.prefs.save(&studio.host.dirs);
                }
                7 => {
                    studio.prefs.generate.deep = Some(on);
                    studio.prefs.save(&studio.host.dirs);
                }
                8 => {
                    studio.prefs.generate.picture_check = Some(on);
                    studio.prefs.save(&studio.host.dirs);
                }
                _ => {}
            },
            GenerateMsg::WriteScript => self.write(studio, false),
            GenerateMsg::Create => {
                self.keep(studio);
                // Still being made from when the person was last here: a
                // second run would pay for every shot again.
                if self
                    .job(studio)
                    .is_some_and(|job| crate::runs::of(&job.dir).is_some())
                {
                    self.message = t("generate.stillMaking");
                    return;
                }
                self.mended = false;
                match self.board.clone() {
                    Some(board) => self.make(studio, board),
                    None => self.write(studio, true),
                }
            }
            GenerateMsg::EditScript => {
                if self.board.is_some() {
                    self.script_open = true;
                    self.script_message.clear();
                    self.load_checks(studio);
                    self.thumbnails(studio, None);
                }
            }
            GenerateMsg::Remake(chapter, scene, shot) => {
                let stem = self.shot_mut(chapter, scene, shot).map(|(stem, _)| stem);
                if let (Some(stem), Some(job)) = (stem, self.job(studio)) {
                    // Made again from the words as they are now: the shot
                    // is fresh, so closing the editor does not throw it
                    // away as stale.
                    self.stale_shots.remove(&stem);
                    self.keep(studio);
                    if let Some(board) = self.board.clone() {
                        self.remake(studio, job, board, stem);
                    }
                }
            }
            GenerateMsg::RemakeClip => {
                let found = studio
                    .selected_media_path()
                    .and_then(|path| shot_of(Path::new(&path)));
                match found {
                    Some((job, stem)) => match Storyboard::read(&job.storyboard()) {
                        Ok(board) => self.remake(studio, job, board, stem),
                        Err(error) => studio.notify(&error, true),
                    },
                    None => studio.notify(&t("generate.notAShot"), true),
                }
            }
            GenerateMsg::PictureModels(maker, key, result) => {
                self.picture_models_came(studio, maker, key, *result)
            }
            GenerateMsg::MakeShort(index) => self.make_short(studio, index.max(0) as usize),
            GenerateMsg::Publish => self.publish(studio),
            GenerateMsg::Tick => {}
            GenerateMsg::Pill => {
                let here = self.job(studio).map(|job| job.dir);
                match crate::runs::latest() {
                    // The run is this project's: its sheet, as it was.
                    Some(run) if here.as_ref() == Some(&run.dir) || self.running => {
                        studio.handle(Msg::Generate(GenerateMsg::Open));
                    }
                    Some(run) => studio.notify(&tf("generate.runElsewhere", &[&run.title]), false),
                    None => {}
                }
            }
            GenerateMsg::Over(dir, title, done) => {
                // This sheet's own run hears of it as Made, and cuts it.
                if !self.running {
                    let here = self.job(studio).map(|job| job.dir);
                    if here.as_ref() == Some(&dir) {
                        self.look(studio);
                        if done {
                            self.message.clear();
                        }
                    }
                    if done {
                        studio.notify(&tf("generate.madeAway", &[&title]), false);
                    }
                }
            }
            GenerateMsg::Published(result) => {
                self.publishing_busy = false;
                match *result {
                    Ok((publishing, picture)) => {
                        match picture {
                            Ok(picture) => {
                                let name = format!(
                                    "{} · {}",
                                    publishing.title.trim(),
                                    t("generate.thumbnail")
                                );
                                // The thumbnail on a timeline of its own,
                                // and the window back on the one that was
                                // open, the sheet left up with the title
                                // and description: making them took the
                                // video off the screen (the owner, 21 Nov).
                                let open = studio.project().active_timeline_id.clone();
                                let mut editing = Session(studio);
                                match plan::thumbnail(
                                    &mut editing,
                                    &name,
                                    &picture,
                                    &publishing.thumbnail_text,
                                ) {
                                    Ok(_) => {
                                        studio.apply(concat_project::Command::SelectTimeline {
                                            timeline_id: open,
                                        });
                                        studio.notify(&t("generate.thumbnailReady"), false);
                                    }
                                    Err(error) => self.message = error,
                                }
                            }
                            Err(error) => {
                                log::warn!("generate: the thumbnail: {error}");
                                studio.notify(&tf("generate.noThumbnail", &[&error]), true);
                            }
                        }
                        self.refresh_shorts_tags(studio, &publishing.tags);
                        self.publishing = Some(publishing);
                    }
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::Act(kind) => match kind {
                0 => self.suggest_ideas(studio),
                1 => {
                    self.queue = self.ideas.iter().map(|idea| idea.title.clone()).collect();
                    self.next(studio);
                }
                2 => self.translate(studio),
                3 => {
                    let prefs = &mut studio.prefs.generate;
                    let name = match (prefs.channel_name.trim(), self.topic.trim()) {
                        ("", "") => format!("{}", prefs.templates.len() + 1),
                        ("", topic) => topic.to_owned(),
                        (channel, _) => channel.to_owned(),
                    };
                    let template = prefs.as_template(&name);
                    prefs.keep_template(template);
                    studio.prefs.save(&studio.host.dirs);
                    studio.notify(&tf("generate.seriesSaved", &[&name]), false);
                }
                4 => self.plan_shorts(studio),
                5 => self.research_first(studio),
                6 => self.export_all(studio),
                8 => self.find_niches(studio),
                10 => self.design_look(studio),
                11 => self.plan_month(studio),
                12 => self.make_plan(studio),
                13 => self.connect_youtube(studio),
                14 => self.upload_last(studio),
                15 => self.read_numbers(studio),
                16 => {
                    let prefs = &mut studio.prefs.generate;
                    prefs.youtube_token.clear();
                    prefs.youtube_channel.clear();
                    studio.prefs.save(&studio.host.dirs);
                }
                9 => self.niche_open = !self.niche_open,
                7 => {
                    let found = self.check_before_publishing(studio);
                    match found.is_empty() {
                        true => studio.notify(&t("generate.checkClean"), false),
                        false => studio.notify(&tf("generate.checkFound", &[&found.len()]), true),
                    }
                }
                _ => {}
            },
            GenerateMsg::ExportNext => self.export_next(studio),
            GenerateMsg::Niches(result) => {
                self.running = false;
                match *result {
                    Ok(niches) => self.niches = niches,
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::ChooseNiche(index) => self.choose_niche(studio, index.max(0) as usize),
            GenerateMsg::YouTubeConnected(result) => {
                self.running = false;
                match *result {
                    Ok((token, channel)) => {
                        let prefs = &mut studio.prefs.generate;
                        prefs.youtube_token = token;
                        prefs.youtube_channel = channel.clone();
                        studio.prefs.save(&studio.host.dirs);
                        studio.notify(&tf("generate.youtubeConnected", &[&channel]), false);
                    }
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::UploadProgress(share) => {
                if self.uploading.is_some() {
                    self.uploading = Some(share);
                }
            }
            GenerateMsg::Uploaded(result) => {
                self.uploading = None;
                match *result {
                    Ok((id, title)) => {
                        let dir = identity::folder(
                            &studio.host.dirs.data,
                            &studio.prefs.generate.channel_name,
                        );
                        if let Err(error) = keep_upload(&dir, &id, &title) {
                            log::warn!("generate: the uploads list: {error}");
                        }
                        studio.notify(&tf("generate.uploaded", &[&title]), false);
                    }
                    Err(error) => studio.notify(&tf("generate.uploadFailed", &[&error]), true),
                }
            }
            GenerateMsg::YouTubeNumbers(result) => {
                self.running = false;
                match *result {
                    Ok(numbers) if numbers.is_empty() => {
                        self.youtube_numbers = t("generate.noUploads")
                    }
                    Ok(numbers) => {
                        self.youtube_numbers = numbers
                            .iter()
                            .map(|video| {
                                tf(
                                    "generate.videoNumbers",
                                    &[&video.title, &video.views, &video.likes, &video.comments],
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::Planned(result) => {
                self.running = false;
                match *result {
                    Ok(calendar) => {
                        let dir = identity::folder(
                            &studio.host.dirs.data,
                            &studio.prefs.generate.channel_name,
                        );
                        if let Err(error) = calendar.keep(&dir) {
                            log::warn!("generate: the channel's plan: {error}");
                        }
                        self.calendar = Some(calendar);
                    }
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::UsePlanned(index) => {
                let title = self
                    .calendar
                    .as_ref()
                    .and_then(|calendar| calendar.entries.get(index.max(0) as usize))
                    .map(|entry| entry.title.clone());
                if let Some(title) = title {
                    self.keep(studio);
                    self.topic = title;
                    self.notes.clear();
                    self.look(studio);
                }
            }
            GenerateMsg::Looked(result) => {
                self.running = false;
                match *result {
                    Ok((look, drawn)) => self.take_look(studio, look, drawn),
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::Researched(result) => {
                self.running = false;
                match *result {
                    Ok(research) => self.research = Some(research),
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::Ideas(result) => {
                self.running = false;
                match *result {
                    Ok(ideas) => self.ideas = ideas,
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::Next => self.next(studio),
            GenerateMsg::Translated(result) => {
                self.running = false;
                match *result {
                    Ok((topic, board, language)) => {
                        // The new video is the sheet's now, in its language.
                        self.topic = topic;
                        studio.prefs.generate.language = language;
                        studio.prefs.save(&studio.host.dirs);
                        self.board = Some(board.clone());
                        self.dirty = false;
                        self.checked = None;
                        self.changed();
                        self.mended = false;
                        self.make(studio, board);
                    }
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::ShortsPlanned(result) => {
                self.running = false;
                match *result {
                    Ok(plan) => {
                        if let Some(job) = self.job(studio)
                            && let Err(error) = plan.keep(&job.dir)
                        {
                            log::warn!("generate: the Shorts' plan: {error}");
                        }
                        self.plan = Some(plan);
                    }
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::ShortMade(index, result) => {
                self.running = false;
                self.making_short = None;
                match *result {
                    Ok((board, made)) => self.short_project(studio, index, &board, &made),
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::OpenShort(index) => {
                let project = self
                    .plan
                    .as_ref()
                    .and_then(|plan| plan.shorts.get(index.max(0) as usize))
                    .map(|short| short.project.clone())
                    .filter(|project| !project.is_empty());
                if let Some(project) = project {
                    self.keep(studio);
                    match switch_project(studio, &project) {
                        // The sheet is the video's, and the video's project
                        // is closed: it starts again, as on any close.
                        Ok(()) => self.closed(),
                        Err(error) => studio.notify(&error, true),
                    }
                }
            }
            GenerateMsg::OpenVideo => {
                // The long video's timeline is the one named after it.
                let title = self
                    .board
                    .as_ref()
                    .map(|board| board.title.trim().to_owned());
                let id = studio
                    .project()
                    .timelines
                    .iter()
                    .rev()
                    .find(|timeline| Some(timeline.name.trim().to_owned()) == title)
                    .map(|timeline| timeline.id.clone());
                if let Some(id) = id {
                    studio.apply(concat_project::Command::SelectTimeline { timeline_id: id });
                }
                self.keep(studio);
                self.open = false;
            }
            GenerateMsg::Suggest(chapter, scene, shot) => {
                self.suggest(studio, chapter, scene, shot)
            }
            GenerateMsg::Suggested(chapter, scene, shot, result) => {
                self.suggesting.remove(&(chapter, scene, shot));
                match *result {
                    Ok(suggestion) => {
                        let (c, s, n) = (chapter as i32, scene as i32, shot as i32);
                        if let Some((stem, found)) = self.shot_mut(c, s, n) {
                            found.prompt = suggestion.prompt;
                            if found.kind == ShotKind::Video && !suggestion.motion.is_empty() {
                                found.motion = suggestion.motion;
                            }
                            self.stale_shots.insert(stem);
                            self.dirty = true;
                            self.changed();
                        }
                    }
                    Err(error) => self.script_message = error,
                }
            }
            GenerateMsg::Designed(chapter, scene, shot, result) => {
                self.suggesting.remove(&(chapter, scene, shot));
                match *result {
                    Ok(design) => {
                        let (c, s, n) = (chapter as i32, scene as i32, shot as i32);
                        if let Some((_, found)) = self.shot_mut(c, s, n) {
                            // Drawn again by the next run, or by Make again:
                            // a design that differs is a clip that differs.
                            found.explainer = Some(design);
                            self.dirty = true;
                            self.changed();
                        }
                    }
                    Err(error) => self.script_message = error,
                }
            }
            GenerateMsg::Remade(stem, dir, result) => {
                self.remaking.remove(&stem);
                self.remade(studio, &stem, &Job { dir }, *result);
            }
            GenerateMsg::Thumbs(thumbs, last) => {
                for (stem, width, height, rgba, likeness) in thumbs {
                    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &rgba, width, height,
                    );
                    self.thumbs
                        .insert(stem.clone(), slint::Image::from_rgba8(buffer));
                    match likeness {
                        Some(likeness) => self.likeness.insert(stem, likeness),
                        None => self.likeness.remove(&stem),
                    };
                }
                self.find_lookalikes();
                // Said once a run's pictures are all read; the script
                // editor shows each.
                if last && !self.script_open && !self.lookalikes.is_empty() {
                    studio.notify(&tf("generate.lookalikes", &[&self.lookalikes.len()]), false);
                }
            }
            GenerateMsg::CloseScript => {
                self.keep(studio);
                self.script_open = false;
                self.tally(studio);
            }
            GenerateMsg::ScriptEdited(chapter, scene, shot, field, text) => {
                self.edit(chapter, scene, shot, field, text);
            }
            GenerateMsg::ShotKindChanged(chapter, scene, shot, video) => {
                if let Some((stem, found)) = self.shot_mut(chapter, scene, shot) {
                    found.kind = if video {
                        ShotKind::Video
                    } else {
                        ShotKind::Image
                    };
                    self.stale_shots.insert(stem);
                    self.dirty = true;
                }
            }
            GenerateMsg::RewriteScene(chapter, scene) => self.rewrite(studio, chapter, scene),
            GenerateMsg::Rewritten(chapter, scene, result) => {
                self.rewriting = None;
                match *result {
                    Ok(new) => {
                        if let Some(board) = self.board.as_mut()
                            && let Some(old) = board
                                .chapters
                                .get_mut(chapter)
                                .and_then(|c| c.scenes.get_mut(scene))
                        {
                            self.stale_voices.insert(old.id.clone());
                            for n in 0..old.shots.len().max(new.shots.len()) {
                                self.stale_shots.insert(shot_stem(&old.id, n));
                            }
                            *old = new;
                            self.dirty = true;
                            self.changed();
                        }
                    }
                    Err(error) => self.script_message = error,
                }
            }
            GenerateMsg::OpenFolder => {
                if let Some(job) = self.job(studio)
                    && let Err(error) = crate::platform::reveal(&job.storyboard().to_string_lossy())
                {
                    studio.notify(&error, true);
                }
            }
            GenerateMsg::Cancel => {
                self.stop.store(true, Ordering::Release);
                if let Some(job) = self.job(studio) {
                    crate::runs::stop(&job.dir);
                }
                studio.host.speech.cancel();
                self.stage = t("generate.stopping");
            }
            GenerateMsg::Progress(progress, stage, detail) => {
                if self.running {
                    self.progress = progress;
                    self.stage = stage;
                    self.detail = detail;
                }
            }
            GenerateMsg::Step(step) => {
                if self.running {
                    self.step(step);
                }
            }
            GenerateMsg::Written(result, then_make) => {
                self.running = false;
                match *result {
                    Ok(board) => {
                        self.board = Some(board.clone());
                        self.dirty = false;
                        self.checked = None;
                        self.changed();
                        // The channel's memory: the next script is told
                        // this one was made.
                        studio.prefs.generate.remember(&board.title);
                        studio.prefs.save(&studio.host.dirs);
                        // A video of the channel's plan, made: marked so.
                        if let Some(calendar) = self.calendar.as_mut()
                            && calendar.mark_made(&self.topic)
                        {
                            let dir = identity::folder(
                                &studio.host.dirs.data,
                                &studio.prefs.generate.channel_name,
                            );
                            if let Err(error) = calendar.keep(&dir) {
                                log::warn!("generate: the channel's plan: {error}");
                            }
                        }
                        // The research it was written from, kept with it.
                        if let Some(job) = self.job(studio) {
                            self.research = job.research();
                        }
                        // What the editor's review found, and mended.
                        if let Some(review) = board
                            .editorial
                            .as_ref()
                            .and_then(|editorial| editorial.review.as_ref())
                        {
                            for issue in &review.issues {
                                log::info!(
                                    "generate: review: [{}] {}: {}",
                                    issue.scene,
                                    issue.kind,
                                    issue.problem
                                );
                            }
                            studio.notify(
                                &tf(
                                    "generate.reviewed",
                                    &[&review.issues.len(), &review.revised.len()],
                                ),
                                false,
                            );
                        }
                        if then_make {
                            self.make(studio, board);
                        } else {
                            // What was written is shown at once, to read
                            // and edit before a shot is paid for.
                            self.script_open = true;
                            self.script_message.clear();
                            self.tally(studio);
                            self.load_checks(studio);
                            self.thumbnails(studio, None);
                        }
                    }
                    Err(error) => self.failed(error),
                }
            }
            GenerateMsg::Made(result) => {
                self.running = false;
                self.ended = Some(std::time::Instant::now());
                match *result {
                    Ok((board, made)) => {
                        // The script as made: a picture check keeps the
                        // prompts it drew again with.
                        self.board = Some(board.clone());
                        // Before the cut: a shot a model could not make is
                        // asked for once more, as an editor would, rather
                        // than the video going out with a card in it.
                        if !self.mended
                            && studio.prefs.generate.shots == 1
                            && review::stood_in(&made.stand_ins) > 0
                        {
                            self.mended = true;
                            studio.notify(&t("generate.reviewMending"), false);
                            self.make(studio, board);
                            return;
                        }
                        self.cut(studio, &board, &made, false);
                        // Read for pictures too like their neighbours.
                        self.load_checks(studio);
                        self.thumbnails(studio, None);
                    }
                    Err(error) => self.failed(error),
                }
                self.tally(studio);
            }
        }
    }

    /// A worker's failure: said on the sheet, unless it was asked for.
    fn failed(&mut self, error: String) {
        if !error.contains("cancel") {
            self.message = error;
        }
    }

    /// The job the idea names in the open project, if one is open.
    fn job(&self, studio: &Studio) -> Option<Job> {
        let project = studio.session.as_ref()?.path().to_owned();
        Some(Job::new(Path::new(&project), self.topic.trim()))
    }

    /// Reads the storyboard already written for the idea, if there is one.
    fn look(&mut self, studio: &Studio) {
        self.board = self
            .job(studio)
            .filter(|_| !self.topic.trim().is_empty())
            .and_then(|job| Storyboard::read(&job.storyboard()).ok());
        self.publishing = self
            .board
            .as_ref()
            .and(self.job(studio))
            .and_then(|job| job.publishing());
        self.research = self
            .job(studio)
            .filter(|_| !self.topic.trim().is_empty())
            .and_then(|job| job.research());
        // Each Short as made into its own project, or not yet.
        self.plan = self
            .board
            .as_ref()
            .and(self.job(studio))
            .and_then(|job| shorts::Plan::read(&job.dir));
        // The findings were for the video loaded before.
        self.checked = None;
        self.dirty = false;
        self.stale_voices.clear();
        self.stale_shots.clear();
        self.changed();
        self.tally(studio);
    }

    /// Works out again how far the job is, from what its folder holds.
    fn tally(&mut self, studio: &Studio) {
        let (Some(board), Some(job)) = (self.board.as_ref(), self.job(studio)) else {
            self.status.clear();
            return;
        };
        let Status {
            voices,
            voices_made,
            shots,
            shots_made,
        } = job.status(board, self.voice_row(studio) != 0);
        self.all_made = shots == shots_made && shots > 0;
        self.status = if shots_made == 0 && voices_made == 0 {
            t("generate.nextStep")
        } else if shots_made == shots && voices_made == voices {
            tf("generate.allMade", &[&shots])
        } else {
            tf("generate.madeSoFar", &[&shots_made, &shots])
        };
    }

    /// Takes in where the making is.
    fn step(&mut self, step: Step) {
        let (fraction, stage) = match &step {
            Step::Voice { n, of, text } => {
                self.phase = 1;
                self.detail = text.clone();
                (
                    0.4 * *n as f32 / *of as f32,
                    tf("generate.readingScene", &[n, of]),
                )
            }
            Step::Shot { n, of, prompt } => {
                self.phase = 2;
                self.detail = prompt.clone();
                self.counts = format!("{} / {of}", n - 1);
                (
                    0.4 + 0.55 * (*n - 1) as f32 / *of as f32,
                    tf("generate.makingShot", &[n, of]),
                )
            }
            Step::Made {
                n,
                of,
                picture,
                spent,
            } => {
                self.phase = 2;
                self.counts = format!("{n} / {of}");
                self.spent = *spent;
                if let Some(image) = picture
                    .as_ref()
                    .and_then(|path| slint::Image::load_from_path(path).ok())
                {
                    self.preview = Some(image);
                }
                (0.4 + 0.55 * *n as f32 / *of as f32, self.stage.clone())
            }
            Step::Music => {
                self.detail.clear();
                (0.95, t("generate.makingMusic"))
            }
            Step::Probing => {
                self.phase = 3;
                self.detail.clear();
                (0.97, t("generate.readingFiles"))
            }
            Step::Checking { n, of } => {
                self.phase = 3;
                self.detail.clear();
                (
                    0.97 + 0.03 * *n as f32 / (*of).max(1) as f32,
                    tf("generate.checkingPicture", &[n, of]),
                )
            }
            Step::Redrawing { n, of } => {
                self.phase = 3;
                self.detail.clear();
                (
                    0.97 + 0.03 * *n as f32 / (*of).max(1) as f32,
                    tf("generate.redrawingPicture", &[n, of]),
                )
            }
        };
        self.progress = fraction;
        self.stage = stage;
    }

    /// The project closed: a run under way stops between two files, so
    /// what it made is kept, and the sheet forgets the idea - opened in the
    /// next project it finds that project's own last job.
    pub fn closed(&mut self) {
        // A run goes on in its job's folder, holding the phone awake on
        // its own (see `runs`); a shot made again stops.
        if !self.running {
            self.stop.store(true, Ordering::Release);
        }
        if self.kept_alive {
            crate::platform::keep_alive(None);
        }
        *self = GeneratePane {
            minutes: self.minutes,
            captions: self.captions,
            titles: self.titles,
            // Export everything goes on across the projects it opens.
            exports: std::mem::take(&mut self.exports),
            export_back: self.export_back.take(),
            export_home: self.export_home.take(),
            export_dir: self.export_dir.take(),
            export_started: self.export_started,
            ..GeneratePane::default()
        };
    }

    /// Works out again what follows from the storyboard: which shots repeat.
    fn changed(&mut self) {
        self.repeats = self
            .board
            .as_ref()
            .map(|board| {
                board
                    .repeats()
                    .into_iter()
                    .map(|(_, later)| later)
                    .collect()
            })
            .unwrap_or_default();
    }

    /// Works out again which shots came out too like the shot before
    /// them, from the likeness of each picture read so far.
    fn find_lookalikes(&mut self) {
        let Some(board) = self.board.as_ref() else {
            self.lookalikes.clear();
            return;
        };
        let stems: Vec<String> = board
            .scenes()
            .flat_map(|scene| (0..scene.shots.len()).map(|n| shot_stem(&scene.id, n)))
            .collect();
        self.lookalikes = stems
            .windows(2)
            .filter(
                |pair| match (self.likeness.get(&pair[0]), self.likeness.get(&pair[1])) {
                    (Some(before), Some(this)) => before.alike(this),
                    _ => false,
                },
            )
            .map(|pair| pair[1].clone())
            .collect();
    }

    /// Writes the edited storyboard back to the job's folder, and removes
    /// what the edits made stale, so the next run makes it again.
    fn keep(&mut self, studio: &mut Studio) {
        if !self.dirty {
            return;
        }
        let Some(job) = self.job(studio) else {
            return;
        };
        let Some(board) = self.board.as_mut() else {
            return;
        };
        match board.tidy().and_then(|()| job.keep(board)) {
            Ok(()) => {
                let voices: Vec<String> =
                    std::mem::take(&mut self.stale_voices).into_iter().collect();
                let shots: Vec<String> =
                    std::mem::take(&mut self.stale_shots).into_iter().collect();
                job.forget(&voices, &shots);
                self.dirty = false;
            }
            Err(error) => studio.notify(&error, true),
        }
        self.changed();
    }

    /// The shot `shot` of `scene` of `chapter`, with its stem.
    fn shot_mut(
        &mut self,
        chapter: i32,
        scene: i32,
        shot: i32,
    ) -> Option<(String, &mut concat_generate::storyboard::Shot)> {
        let scene = self
            .board
            .as_mut()?
            .chapters
            .get_mut(usize::try_from(chapter).ok()?)?
            .scenes
            .get_mut(usize::try_from(scene).ok()?)?;
        let n = usize::try_from(shot).ok()?;
        let stem = shot_stem(&scene.id, n);
        scene.shots.get_mut(n).map(|found| (stem, found))
    }

    /// One text of the script changed; see the editor's `edited`.
    fn edit(&mut self, chapter: i32, scene: i32, shot: i32, field: i32, text: String) {
        if shot >= 0 {
            let Some((stem, found)) = self.shot_mut(chapter, scene, shot) else {
                return;
            };
            // An explainer's prompt is what Design again designs from:
            // the clip changes when its design does.
            if found.kind == ShotKind::Explainer && found.explainer.is_some() {
                if field == 0 {
                    found.prompt = text;
                    self.dirty = true;
                }
                return;
            }
            // A card's words are drawn by the cut from the script as it
            // stands: nothing to make again.
            if let (ShotKind::Motion, Some(graphic)) = (found.kind, found.graphic.as_mut()) {
                match field {
                    0 => graphic.title = text,
                    1 => graphic.items = read_card_lines(&text),
                    _ => return,
                }
                self.dirty = true;
                return;
            }
            match field {
                0 => found.prompt = text,
                1 => found.motion = text,
                _ => return,
            }
            self.stale_shots.insert(stem);
            self.dirty = true;
            if field == 0 {
                self.changed();
            }
            return;
        }
        let Some(board) = self.board.as_mut() else {
            return;
        };
        let Some(found) = usize::try_from(chapter)
            .ok()
            .and_then(|c| board.chapters.get_mut(c))
        else {
            return;
        };
        if scene < 0 {
            match field {
                0 => found.title = text,
                1 => {
                    // A new mood is a new look for every shot of the chapter.
                    found.mood = text;
                    for scene in &found.scenes {
                        for n in 0..scene.shots.len() {
                            self.stale_shots.insert(shot_stem(&scene.id, n));
                        }
                    }
                }
                2 => found.summary = text,
                _ => return,
            }
            self.dirty = true;
            return;
        }
        let Some(found) = usize::try_from(scene)
            .ok()
            .and_then(|s| found.scenes.get_mut(s))
        else {
            return;
        };
        match field {
            0 => {
                found.narration = text;
                self.stale_voices.insert(found.id.clone());
            }
            1 => {
                found.voice = text;
                self.stale_voices.insert(found.id.clone());
            }
            2 => {
                found.on_screen_text = Some(text).filter(|text| !text.trim().is_empty());
            }
            _ => return,
        }
        self.dirty = true;
    }

    /// The brief the sheet's choices make.
    fn brief(&self, studio: &Studio) -> Brief {
        let prefs = &studio.prefs.generate;
        Brief {
            topic: self.topic.trim().to_owned(),
            minutes: MINUTES[self.minutes.min(MINUTES.len() - 1)],
            language: LANGUAGES[prefs.language.min(LANGUAGES.len() - 1)].to_owned(),
            aspect: ASPECTS[prefs.aspect.min(ASPECTS.len() - 1)].to_owned(),
            notes: self.notes.clone(),
            clip_share: CLIP_SHARES[self.clips_row(studio)],
            style: ContentStyle::ALL[prefs.content_style.min(ContentStyle::ALL.len() - 1)],
            level: Level::ALL[level_row(prefs)],
            channel: channel_profile(prefs),
            research: prefs.research.unwrap_or(true),
            cards_only: prefs.visuals == 1,
            pictures: PictureStyle::ALL[prefs.pictures.min(PictureStyle::ALL.len() - 1)],
            source: self.source.trim().to_owned(),
            format: VideoFormat::ALL[prefs.format.min(VideoFormat::ALL.len() - 1)],
            structure: prefs.structure.trim().to_owned(),
            researched: self
                .research
                .clone()
                .filter(|research| !research.notes.trim().is_empty()),
            hook: prefs.hook.unwrap_or(true),
            deep: prefs.deep.unwrap_or(false),
        }
    }

    /// What the video will cost: the storyboard's own counts once it is
    /// written, the budget's before.
    fn estimate(&self, studio: &Studio) -> Estimate {
        let prefs = &studio.prefs.generate;
        let totals = match &self.board {
            Some(board) => {
                let (clips, stills) = board.kinds();
                Totals {
                    seconds: board.seconds(),
                    chapters: board.chapters.len(),
                    scenes: board.scenes().count(),
                    clips,
                    stills,
                    words: board.words(),
                }
            }
            None => budget::budget(&self.brief(studio).pacing()).totals(),
        };
        let mut estimate = cost::estimate(
            &totals,
            &prices(studio),
            true,
            // A model's inputs are not known before the run reads them:
            // count the still a clip may start from, which costs little.
            true,
            self.voice_row(studio) == 2,
        );
        // Shots from Replicate are paid for; draft cards, shots made by
        // hand and animated cards are not.
        if prefs.shots != 1 || (self.board.is_none() && prefs.visuals == 1) {
            estimate.stills = 0.0;
            estimate.clips = 0.0;
        }
        estimate
    }

    /// The estimate in a line, and over the limit when it is.
    fn cost_line(&self, studio: &Studio) -> String {
        let estimate = self.estimate(studio);
        let mut line = tf(
            "generate.cost",
            &[
                &dollars(estimate.total()),
                &dollars(estimate.script),
                &dollars(estimate.stills),
                &dollars(estimate.clips),
                &dollars(estimate.voice),
            ],
        );
        match spend_limit(studio) {
            Some(limit) if estimate.total() > limit => {
                line.push(' ');
                line.push_str(&tf("generate.overLimit", &[&dollars(limit)]));
            }
            None if !studio.prefs.generate.spend_limit.trim().is_empty() => {
                line.push(' ');
                line.push_str(&t("generate.limitNotNumber"));
            }
            _ => {}
        }
        line
    }

    /// Row of the clips list.
    fn clips_row(&self, studio: &Studio) -> usize {
        studio
            .prefs
            .generate
            .clips
            .unwrap_or(DEFAULT_CLIPS)
            .min(CLIP_SHARES.len() - 1)
    }

    /// Asks Claude for one scene again, on a worker.
    fn rewrite(&mut self, studio: &mut Studio, chapter: i32, scene: i32) {
        let (Ok(chapter), Ok(scene)) = (usize::try_from(chapter), usize::try_from(scene)) else {
            return;
        };
        let Some(board) = self.board.clone() else {
            return;
        };
        if self.rewriting.is_some() {
            return;
        }
        let (key, model) = studio.prefs.generate.writer();
        if key.trim().is_empty() {
            self.script_message = need_writer_key(&studio.prefs.generate);
            return;
        }
        self.script_message.clear();
        self.rewriting = Some((chapter, scene));
        let brief = self.brief(studio);
        spawn_in_project(
            move || writer::rewrite_scene(&board, &brief, chapter, scene, "", false, &key, &model),
            move |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Rewritten(
                    chapter,
                    scene,
                    Box::new(result),
                )))
            },
        );
    }

    /// Asks the writer for another picture for one shot, on a worker; it
    /// takes the shot's words' place when it comes, to read, edit, and
    /// make again if it is better.
    /// Asks the chosen picture maker, on a worker, which picture models
    /// its key can run - once per maker and key, so typing a key asks for
    /// the key as it ends up, not for every letter of it twice.
    fn ask_picture_models(&mut self, studio: &mut Studio) {
        let prefs = &studio.prefs.generate;
        let maker = prefs.picture_maker.min(2);
        let key = match maker {
            1 => prefs.openai_key.trim().to_owned(),
            2 => prefs.gemini_key.trim().to_owned(),
            _ => String::new(),
        };
        let print = fingerprint(&key);
        let wanted = &mut self.picture_models;
        if wanted.maker == maker && wanted.key == print {
            return;
        }
        *wanted = PictureModels {
            maker,
            key: print,
            ..PictureModels::default()
        };
        if maker == 0 || key.is_empty() {
            return;
        }
        wanted.asking = true;
        let pictures = match maker {
            1 => makers::Pictures::OpenAi {
                key: key.clone(),
                model: String::new(),
            },
            _ => makers::Pictures::Gemini {
                key: key.clone(),
                model: String::new(),
            },
        };
        spawn(
            move || makers::picture_models(&pictures),
            move |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::PictureModels(
                    maker,
                    print,
                    Box::new(result),
                )))
            },
        );
    }

    /// The picture models came. A model chosen before that the account
    /// cannot run - a name typed wrong, a model retired - gives way to
    /// the first it can, the default when it has it, so the run does not
    /// fail at its first picture.
    fn picture_models_came(
        &mut self,
        studio: &mut Studio,
        maker: usize,
        key: u64,
        result: Result<Vec<String>, String>,
    ) {
        let wanted = &mut self.picture_models;
        // Asked for a maker or a key since changed: the newer asking
        // answers.
        if wanted.maker != maker || wanted.key != key {
            return;
        }
        wanted.asking = false;
        match result {
            Ok(names) if !names.is_empty() => wanted.names = names,
            Ok(_) => wanted.error = t("generate.noPictureModels"),
            Err(error) => {
                log::warn!("generate: the picture models: {error}");
                wanted.error = error;
            }
        }
        let Some(first) = wanted.names.first().cloned() else {
            return;
        };
        let prefs = &mut studio.prefs.generate;
        let (chosen, default) = match maker {
            1 => (&mut prefs.openai_image_model, makers::OPENAI_IMAGE_MODEL),
            _ => (&mut prefs.gemini_image_model, makers::GEMINI_IMAGE_MODEL),
        };
        let now = if chosen.trim().is_empty() {
            default
        } else {
            chosen.trim()
        };
        if !wanted.names.iter().any(|name| name == now) {
            *chosen = first;
            studio.prefs.save(&studio.host.dirs);
        }
    }

    fn suggest(&mut self, studio: &mut Studio, chapter: i32, scene: i32, shot: i32) {
        let (Ok(chapter), Ok(scene), Ok(shot)) = (
            usize::try_from(chapter),
            usize::try_from(scene),
            usize::try_from(shot),
        ) else {
            return;
        };
        let Some(board) = self.board.clone() else {
            return;
        };
        if !self.suggesting.insert((chapter, scene, shot)) {
            return;
        }
        let (key, model) = studio.prefs.generate.writer();
        if key.trim().is_empty() {
            self.suggesting.remove(&(chapter, scene, shot));
            self.script_message = need_writer_key(&studio.prefs.generate);
            return;
        }
        self.script_message.clear();
        let brief = self.brief(studio);
        // An explainer is designed again, as the director would, from what
        // its prompt says it shows.
        let explainer = board
            .chapters
            .get(chapter)
            .and_then(|c| c.scenes.get(scene))
            .and_then(|s| s.shots.get(shot))
            .is_some_and(|found| found.kind == ShotKind::Explainer);
        if explainer {
            spawn_in_project(
                move || {
                    concat_generate::director::direct(
                        &board, &brief, chapter, scene, shot, "", &key, &model,
                    )
                },
                move |studio, _, _, result| {
                    studio.handle(Msg::Generate(GenerateMsg::Designed(
                        chapter,
                        scene,
                        shot,
                        Box::new(result),
                    )))
                },
            );
            return;
        }
        spawn_in_project(
            move || writer::suggest_shot(&board, &brief, chapter, scene, shot, &key, &model),
            move |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Suggested(
                    chapter,
                    scene,
                    shot,
                    Box::new(result),
                )))
            },
        );
    }

    /// Asks Claude for the storyboard on a worker; `then_make` goes on to
    /// make and cut it once it is written.
    fn write(&mut self, studio: &mut Studio, then_make: bool) {
        let prefs = studio.prefs.generate.clone();
        if self.topic.trim().is_empty() {
            self.message = t("generate.describeIdeaFirst");
            return;
        }
        let (key, model) = prefs.writer();
        if key.trim().is_empty() {
            self.message = need_writer_key(&prefs);
            return;
        }
        let Some(job) = self.job(studio) else { return };
        self.remember(studio, &job);
        let mut brief = self.brief(studio);
        self.begin(t("generate.writingOutline"));
        self.phase = 0;
        let epoch = crate::host::project_epoch();
        spawn_in_project(
            move || {
                // Read up first, when the research is wanted and none is
                // kept, and keep it with the video: the person can read it
                // after, and a script written again is written from it
                // without paying for it again.
                // Read up again for deeper analysis when what is kept was read
                // without it - and the person has not corrected it: their
                // notes are not written over.
                let shallow = brief.deep
                    && brief
                        .researched
                        .as_ref()
                        .is_some_and(|kept| !kept.deep && !kept.edited);
                if shallow {
                    brief.researched = None;
                }
                if brief.researched.is_none() && (brief.research || !brief.source.trim().is_empty())
                {
                    on_ui_in_project(epoch, |studio, _, _| {
                        studio.handle(Msg::Generate(GenerateMsg::Progress(
                            0.01,
                            t("generate.researching"),
                            String::new(),
                        )));
                    });
                    match writer::research(&brief, &key, &model) {
                        Ok(found) => {
                            if let Err(error) = job.keep_research(&found) {
                                log::warn!("generate: the research: {error}");
                            }
                            brief.researched = Some(found);
                        }
                        // Written from what the writer knows, as before.
                        Err(error) => {
                            log::warn!("research: {error}; writing without it");
                            brief.research = false;
                            brief.source.clear();
                        }
                    }
                }
                let board = writer::write(&brief, &key, &model, |at| {
                    let (fraction, stage, detail) = match at {
                        Writing::Research => (0.01, t("generate.researching"), String::new()),
                        Writing::Analysis => (0.015, t("generate.analysing"), String::new()),
                        Writing::Outline => (0.02, t("generate.writingOutline"), String::new()),
                        Writing::Chapter { n, of, title } => (
                            n as f32 / (of + 1) as f32,
                            tf("generate.writingChapter", &[&n, &of]),
                            title,
                        ),
                        Writing::Explainer {
                            chapter,
                            chapters,
                            n,
                            of,
                        } => (
                            chapter as f32 / (chapters + 1) as f32,
                            tf("generate.designingExplainer", &[&n, &of]),
                            String::new(),
                        ),
                        Writing::Hook => (0.96, t("generate.writingHook"), String::new()),
                        Writing::Review => (0.97, t("generate.reviewing"), String::new()),
                        Writing::Revising { n, of } => (
                            0.97 + 0.03 * n as f32 / (of + 1) as f32,
                            tf("generate.revising", &[&n, &of]),
                            String::new(),
                        ),
                    };
                    on_ui_in_project(epoch, move |studio, _, _| {
                        studio.handle(Msg::Generate(GenerateMsg::Progress(
                            fraction, stage, detail,
                        )));
                    });
                })?;
                let old = Storyboard::read(&job.storyboard()).ok();
                job.keep(&board)?;
                if let Some(old) = old {
                    job.retire(&old, &board);
                }
                Ok(board)
            },
            move |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Written(
                    Box::new(result),
                    then_make,
                )))
            },
        );
    }

    /// Makes the shot `stem` of `board` again, on a worker: a new take,
    /// which then takes the old one's place on the timeline.
    fn remake(&mut self, studio: &mut Studio, job: Job, board: Storyboard, stem: String) {
        if self.remaking.contains(&stem) {
            return;
        }
        let prefs = studio.prefs.generate.clone();
        let shots = shots_of(&prefs);
        let costs = Costs {
            prices: prices(studio),
            limit: spend_limit(studio),
        };
        self.script_message.clear();
        self.remaking.insert(stem.clone());
        let dir = job.dir.clone();
        spawn_in_project(
            move || {
                // Probed here, off the window's thread, the way an import
                // is: the cut takes the take as it takes any file.
                let result = job
                    .remake(&board, &stem, &shots, &costs)
                    .and_then(|remade| {
                        let summary = concat_host::media::probe(&remade.path.to_string_lossy())?;
                        Ok((remade, summary.to_new_media()))
                    });
                (stem, result)
            },
            move |studio, _, _, (stem, result)| {
                studio.handle(Msg::Generate(GenerateMsg::Remade(
                    stem,
                    dir,
                    Box::new(result),
                )))
            },
        );
    }

    /// A shot made again: the new take takes the place of every clip that
    /// showed the shot, as one step Undo takes back, and its picture is
    /// shown in the script editor.
    fn remade(
        &mut self,
        studio: &mut Studio,
        stem: &str,
        job: &Job,
        result: Result<(Remade, NewMedia), String>,
    ) {
        // A new take is a picture the last check never saw.
        if result.is_ok() {
            self.picture_checks.remove(stem);
        }
        let (remade, item) = match result {
            Ok(made) => made,
            Err(error) => {
                if self.script_open {
                    self.script_message = error;
                } else {
                    studio.notify(&error, true);
                }
                return;
            }
        };
        // Every clip on the timeline that shows this shot - the take first
        // made, or one since - and is a picture of the same kind: a still
        // stays a still, a clip a clip.
        let still = is_still(&remade.path);
        let shows = |path: &str| {
            let path = Path::new(path);
            is_still(path) == still
                && shot_of(path)
                    .is_some_and(|(found, found_stem)| found.dir == job.dir && found_stem == stem)
        };
        let media: HashSet<&str> = studio
            .project()
            .media
            .iter()
            .filter(|media| shows(&media.path))
            .map(|media| media.id.as_str())
            .collect();
        let commands: Vec<Command> = studio
            .timeline()
            .clips
            .iter()
            .filter(|clip| media.contains(clip.media_id.as_str()))
            .map(|clip| Command::ReplaceClipMedia {
                clip_id: clip.id.clone(),
                item: item.clone(),
                source_start: None,
                enhanced: false,
            })
            .collect();
        if !commands.is_empty() {
            studio.apply(Command::Batch { commands });
            studio.request_media_art();
            studio.request_preview();
        }
        studio.notify(&t("generate.madeAgain"), false);
        if remade.spent > 0.0 {
            studio.notify(&tf("generate.spent", &[&dollars(remade.spent)]), false);
        }
        self.thumbnails(studio, Some(stem.to_owned()));
        self.tally(studio);
    }

    /// Makes small pictures of the shots made - every one, or `only` that
    /// one - on a worker, for the script editor. They are handed over a few
    /// at a time, so a long video's first shots show while the rest are
    /// still being read.
    /// Reads the last picture check kept with the job, for the script
    /// editor; none when no check ran.
    fn load_checks(&mut self, studio: &Studio) {
        self.picture_checks = self
            .job(studio)
            .and_then(|job| concat_generate::visual::read_report(&job))
            .map(|report| {
                report
                    .checks
                    .into_iter()
                    .map(|check| (check.stem.clone(), check))
                    .collect()
            })
            .unwrap_or_default();
    }

    fn thumbnails(&self, studio: &Studio, only: Option<String>) {
        let (Some(board), Some(job)) = (self.board.as_ref(), self.job(studio)) else {
            return;
        };
        let stems: Vec<String> = match only {
            Some(stem) => vec![stem],
            None => board
                .scenes()
                .flat_map(|scene| (0..scene.shots.len()).map(|n| shot_stem(&scene.id, n)))
                .collect(),
        };
        let epoch = crate::host::project_epoch();
        spawn_in_project(
            move || {
                let batches = stems.chunks(THUMBS_AT_ONCE).count();
                for (n, stems) in stems.chunks(THUMBS_AT_ONCE).enumerate() {
                    let thumbs: Vec<_> = stems
                        .iter()
                        .filter_map(|stem| {
                            let path = job.picture(stem)?;
                            let (small, likeness) = thumbnail(&path)?;
                            let (width, height) = small.dimensions();
                            Some((stem.clone(), width, height, small.into_raw(), likeness))
                        })
                        .collect();
                    let last = n + 1 == batches;
                    if !thumbs.is_empty() || last {
                        on_ui_in_project(epoch, move |studio, _, _| {
                            studio.handle(Msg::Generate(GenerateMsg::Thumbs(thumbs, last)));
                        });
                    }
                }
            },
            |_, _, _, ()| {},
        );
    }

    /// Whether the clip selected shows a shot this sheet made, which can
    /// be made again; and whether it is being.
    pub fn remakable(&self, studio: &Studio) -> (bool, bool) {
        match studio
            .selected_media_path()
            .and_then(|path| shot_of(Path::new(&path)))
        {
            Some((_, stem)) => (true, self.remaking.contains(&stem)),
            None => (false, false),
        }
    }

    /// Keeps the idea in the job's folder, so the sheet finds the job again.
    fn remember(&self, studio: &mut Studio, job: &Job) {
        let idea = Idea {
            topic: self.topic.trim().to_owned(),
            notes: self.notes.clone(),
        };
        if let Err(error) = job.remember(&idea) {
            studio.notify(&error, true);
        }
    }

    /// Makes the narration and the shots on a worker, then cuts.
    fn make(&mut self, studio: &mut Studio, board: Storyboard) {
        let prefs = studio.prefs.generate.clone();
        let Some(job) = self.job(studio) else { return };
        self.remember(studio, &job);
        let Some(project) = studio
            .session
            .as_ref()
            .map(|session| session.path().to_owned())
        else {
            return;
        };
        let voice = match self.voice_row(studio) {
            1 => Voice::BuiltIn,
            2 => Voice::ElevenLabs {
                key: prefs.elevenlabs_key.clone(),
                voice_id: prefs.elevenlabs_voice.clone(),
                model: if prefs.elevenlabs_model == 1 {
                    providers::EXPRESSIVE_VOICE_MODEL.to_owned()
                } else {
                    String::new()
                },
                lexicon: providers::lexicon(&prefs.pronunciation),
            },
            _ => Voice::None,
        };
        let brand = brand_of(&prefs);
        let shots = shots_of(&prefs);
        // The window's own voice: the speech model in use and the speaker
        // chosen last in the speech sheet.
        let model = installed(&studio.settings.voices)
            .iter()
            .find(|model| model.active)
            .or(installed(&studio.settings.voices).first())
            .map(|model| model.id.clone());
        if voice == Voice::BuiltIn && model.is_none() {
            self.message = t("speech.downloadVoiceModelFirst");
            return;
        }
        let costs = Costs {
            prices: prices(studio),
            limit: spend_limit(studio),
        };
        // Music made for the video, where it is asked for and the shots
        // come from Replicate, which makes it; described by the writer
        // once, before the first bed is paid for.
        let ai_music = self.music == AI_MUSIC
            && matches!(&shots, Shots::Models { replicate, .. } if !replicate.trim().is_empty())
            && job.music().is_none();
        let (writer_key, writer_model) = prefs.writer();
        // The pictures looked at against their words once made, with the
        // writer's key: the model that writes the script sees.
        let picture_check = prefs.picture_check.unwrap_or(false);
        let threshold = prefs
            .picture_threshold
            .unwrap_or(concat_generate::visual::DEFAULT_THRESHOLD)
            .min(10);
        let attempts = prefs
            .picture_attempts
            .unwrap_or(concat_generate::visual::DEFAULT_ATTEMPTS)
            .min(3);
        let voiced = !matches!(voice, Voice::None);
        let speaker = studio.prefs.tts_voice.unwrap_or(DEFAULT_SPEAKER);
        let speech = Arc::clone(&studio.host.speech);
        let dirs = studio.host.dirs.clone();

        self.begin(t("generate.makingShots"));
        self.voiced = voice != Voice::None;
        self.phase = if self.voiced { 1 } else { 2 };
        self.counts = format!("0 / {}", board.shot_count());
        // The shots a run before this one made: the first is shown at once.
        self.preview = board
            .scenes()
            .flat_map(|scene| (0..scene.shots.len()).map(|n| shot_stem(&scene.id, n)))
            .filter_map(|stem| job.picture(&stem))
            .last()
            .and_then(|path| slint::Image::load_from_path(&path).ok());
        let stop = Arc::clone(&self.stop);
        let epoch = crate::host::project_epoch();
        // Known by its folder, not by the open project: it goes on when
        // the person opens another, and shows when they come back.
        let title = if board.title.trim().is_empty() {
            self.topic.trim().to_owned()
        } else {
            board.title.trim().to_owned()
        };
        crate::runs::begin(&job.dir, &title, Arc::clone(&self.stop));
        let dir = job.dir.clone();
        spawn_in_project(
            move || {
                let held = crate::runs::Held(dir.clone());
                crate::platform::hold(MAKING, Some(t("generate.keptAlive")), -1);
                let mut speak = |text: &str| -> Result<PathBuf, String> {
                    let request = concat_speech::tts::SpeakRequest {
                        model_id: model.clone().unwrap_or_default(),
                        voice: speaker,
                        reference: None,
                        text: text.to_owned(),
                        speed: 1.0,
                        pauses: Some(0.2),
                        steps: None,
                        project: project.clone(),
                    };
                    speech
                        .speak(&dirs, &request, |_| {})
                        .map(|spoken| PathBuf::from(spoken.path))
                };
                let mut progress = |step: Step| {
                    if let Some((fraction, stage)) = rough(&step) {
                        crate::runs::update(&dir, fraction, &stage);
                        crate::platform::hold(MAKING, Some(stage), (fraction * 100.0) as i32);
                    }
                    on_ui_in_project(epoch, move |studio, _, _| {
                        studio.handle(Msg::Generate(GenerateMsg::Step(step)));
                    });
                    // Drawn again wherever the window is: the pill reads
                    // how far the run is.
                    on_ui(|studio, _, _| studio.handle(Msg::Generate(GenerateMsg::Tick)));
                };
                let music = ai_music.then(|| {
                    writer::music_prompt(&board, &writer_key, &writer_model).unwrap_or_else(
                        |error| {
                            log::warn!("generate: the music's description: {error}");
                            writer::plain_music_prompt()
                        },
                    )
                });
                // The channel's opening and closing, drawn before the run
                // so its cut finds them; a video goes out without them
                // where they cannot be drawn.
                if let Some(brand) = &brand
                    && let Err(error) =
                        job.draw_bumpers(&board, brand, &|| stop.load(Ordering::Acquire))
                {
                    log::warn!("generate: the channel's opening and closing: {error}");
                }
                let made = job.make(
                    &board,
                    &voice,
                    &shots,
                    &costs,
                    music.as_deref(),
                    &mut speak,
                    &mut progress,
                    &|| stop.load(Ordering::Acquire),
                );
                // Every picture a model drew looked at against what is
                // said over it, and the stills that fail drawn again.
                let mut board = board;
                let made = match made {
                    Ok(mut made) if picture_check && !stop.load(Ordering::Acquire) => {
                        let (report, redrawing) = concat_generate::visual::check_and_mend(
                            &job,
                            &mut board,
                            &shots,
                            &costs,
                            &writer_key,
                            &writer_model,
                            threshold,
                            attempts,
                            &mut |redrawing, n, of| {
                                progress(match redrawing {
                                    true => Step::Redrawing { n, of },
                                    false => Step::Checking { n, of },
                                })
                            },
                            &|| stop.load(Ordering::Acquire),
                        );
                        for check in report.checks.iter().filter(|check| !check.is_acceptable) {
                            log::warn!(
                                "generate: picture {} ({}/10): missing {:?}, wrong {:?} {}",
                                check.stem,
                                check.semantic_alignment_score,
                                check.missing_elements,
                                check.visual_mismatches,
                                check.note
                            );
                        }
                        if !report.redrawn.is_empty() {
                            // The new takes, in place of the ones they mend.
                            let spent = made.spent;
                            made = job.gather(&board, voiced)?;
                            made.spent = spent + redrawing;
                        }
                        let (passed, failed, unchecked) = report.counts();
                        let redrawn = report.redrawn.len();
                        on_ui(move |studio, _, _| {
                            studio.notify(
                                &tf(
                                    "generate.picturesChecked",
                                    &[&passed, &failed, &unchecked, &redrawn],
                                ),
                                failed + unchecked > 0,
                            );
                        });
                        Ok(made)
                    }
                    made => made,
                };
                drop(held);
                crate::platform::hold(MAKING, None, -1);
                // Said wherever the window is now: a run left behind in a
                // project the person went away from is over.
                let done = made.is_ok();
                on_ui(move |studio, _, _| {
                    studio.handle(Msg::Generate(GenerateMsg::Over(dir, title, done)));
                });
                Ok((board, made?))
            },
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Made(Box::new(result))))
            },
        );
    }

    /// Writes what goes up with the video on YouTube - a title, a
    /// description with the chapters' times, tags - and makes its
    /// thumbnail's picture, on a worker: kept in the job's folder, and the
    /// thumbnail laid on a timeline of its own when it comes back.
    fn publish(&mut self, studio: &mut Studio) {
        let (Some(board), Some(job)) = (self.board.clone(), self.job(studio)) else {
            return;
        };
        let prefs = studio.prefs.generate.clone();
        let (key, model) = prefs.writer();
        if key.trim().is_empty() {
            self.message = need_writer_key(&prefs);
            return;
        }
        self.message.clear();
        self.publishing_busy = true;
        let shots = shots_of(&prefs);
        let voiced = self.voice_row(studio) != 0;
        let thumbnail_style = prefs.thumbnail_style.clone();
        spawn_in_project(
            move || {
                let mut publishing = writer::publishing(&board, &key, &model)?;
                // Timed as the cut is; a video not made yet has no times.
                publishing.chapters = job
                    .gather(&board, voiced)
                    .ok()
                    .and_then(|made| {
                        plan::plan(&board, &made.files, &Options::default(), None).ok()
                    })
                    .map(|placements| plan::chapter_starts(&board, &placements))
                    .unwrap_or_default();
                job.keep_publishing(&publishing)?;
                // Every thumbnail of the channel in its own style.
                let prompt = match thumbnail_style.trim() {
                    "" => publishing.thumbnail_prompt.clone(),
                    style => format!("{}. {style}", publishing.thumbnail_prompt.trim()),
                };
                let picture = job
                    .thumbnail_picture(&board, &shots, &prompt)
                    .and_then(|path| {
                        concat_host::media::probe(&path.to_string_lossy())
                            .map(|summary| summary.to_new_media())
                    });
                Ok((publishing, picture))
            },
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Published(Box::new(result))))
            },
        );
    }

    /// Asks the writer for videos worth making next: about what is typed,
    /// or whatever suits the channel when nothing is.
    /// Export everything: every timeline of the project that holds
    /// anything - the long video and its Shorts - written one after
    /// another into the video's folder, beside what goes up with each.
    fn export_all(&mut self, studio: &mut Studio) {
        let Some(job) = self.job(studio) else { return };
        let Some(home) = studio
            .session
            .as_ref()
            .map(|session| session.path().to_owned())
        else {
            return;
        };
        if studio.export.phase == crate::ui::ExportPhase::Running || !self.exports.is_empty() {
            studio.notify(&t("generate.exportBusy"), true);
            return;
        }
        let project = studio.project();
        // The thumbnail's timeline is a picture, saved as one, not a video.
        let thumbnail = format!(" · {}", t("generate.thumbnail"));
        let mut taken = HashSet::new();
        let mut exports: Vec<Exporting> = project
            .timelines
            .iter()
            .filter(|timeline| !timeline.clips.is_empty() && !timeline.name.ends_with(&thumbnail))
            .map(|timeline| {
                Exporting::Timeline(timeline.id.clone(), unique_name(&timeline.name, &mut taken))
            })
            .collect();
        // Then each Short made, from its own project, into the video's
        // folder beside the rest.
        exports.extend(
            self.plan
                .iter()
                .flat_map(|plan| &plan.shorts)
                .filter(|short| !short.project.is_empty())
                .map(|short| {
                    Exporting::Short(short.project.clone(), unique_name(&short.title, &mut taken))
                }),
        );
        if exports.is_empty() {
            studio.notify(&t("export.nothingTimelineExport"), true);
            return;
        }
        self.export_back = Some(project.active_timeline_id.clone());
        self.export_home = Some(home);
        // Looked over first: said, never held back on.
        let found = self.check_before_publishing(studio);
        if !found.is_empty() {
            studio.notify(&tf("generate.checkFound", &[&found.len()]), true);
        }
        studio.notify(&tf("generate.exportingAll", &[&exports.len()]), false);
        self.exports = exports;
        let _ = std::fs::create_dir_all(&job.dir);
        self.export_dir = Some(job.dir);
        self.export_next(studio);
    }

    /// What to look at before the videos go up - figures with no source,
    /// disputed points, a title made before, the music's rights, an
    /// upright video too long for Shorts, realistic AI pictures - kept for
    /// the sheet to list and returned. Warnings only: nothing waits on it.
    fn check_before_publishing(&mut self, studio: &Studio) -> Vec<String> {
        let Some(board) = self.board.as_ref() else {
            self.checked = Some(Vec::new());
            return Vec::new();
        };
        let prefs = &studio.prefs.generate;
        let music = match self.music {
            0 => checklist::Music::None,
            // Only when a bed was made: without Replicate's shots there is
            // none, and the cut carries no music.
            AI_MUSIC => match self.job(studio).and_then(|job| job.music()) {
                Some(_) => checklist::Music::Made,
                None => checklist::Music::None,
            },
            row => row
                .checked_sub(AI_MUSIC + 1)
                .and_then(|row| music_items(studio).into_iter().nth(row))
                .map(|(path, _)| {
                    let name = Path::new(&path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or(path);
                    checklist::Music::Own(name)
                })
                .unwrap_or(checklist::Music::None),
        };
        // Photographs or footage made by a model: pictures by a model, not
        // cards alone, in the automatic or the photo style, or any clips.
        let by_model = prefs.shots == 1 && prefs.visuals == 0;
        let realistic =
            by_model && (prefs.pictures <= 1 || CLIP_SHARES[self.clips_row(studio)] > 0.0);
        let thumbnail = format!(" · {}", t("generate.thumbnail"));
        let cuts: Vec<checklist::Cut> = studio
            .project()
            .timelines
            .iter()
            .filter(|timeline| !timeline.clips.is_empty() && !timeline.name.ends_with(&thumbnail))
            .map(|timeline| checklist::Cut {
                name: timeline.name.clone(),
                seconds: timeline.duration(),
                upright: timeline.video.height > timeline.video.width,
            })
            .collect();
        let found: Vec<String> = checklist::check(&checklist::Before {
            board,
            notes: self
                .research
                .as_ref()
                .map(|research| research.notes.as_str()),
            made_before: prefs.made_before(),
            music,
            realistic,
            cuts: &cuts,
        })
        .iter()
        .map(warning_text)
        .collect();
        for line in &found {
            log::info!("generate: before publishing: {line}");
        }
        self.checked = Some(found.clone());
        found
    }

    /// Starts the next video of Export everything once the one before is
    /// written; looks again in a moment while it is not. A file that
    /// failed stops the rest; the export sheet says why.
    fn export_next(&mut self, studio: &mut Studio) {
        // Waited on with `spawn`, not `spawn_in_project`: a Short is
        // written from its own project, and the wait must outlive the
        // switch to it.
        let wait = || {
            spawn(
                || std::thread::sleep(std::time::Duration::from_secs(3)),
                |studio, _, _, ()| studio.handle(Msg::Generate(GenerateMsg::ExportNext)),
            )
        };
        if studio.export.phase == crate::ui::ExportPhase::Running {
            wait();
            return;
        }
        // A file that failed, or one cancelled - the sheet back on its form
        // rather than done - stops the rest.
        let failed = self.export_started
            && matches!(
                studio.export.phase,
                crate::ui::ExportPhase::Failed | crate::ui::ExportPhase::Idle
            );
        if failed || self.exports.is_empty() || self.export_dir.is_none() {
            // Done, or stopped: back on the video's project, on the
            // timeline that was open.
            let written = self.export_started && !failed;
            self.exports.clear();
            self.export_started = false;
            self.export_dir = None;
            if let Some(home) = self.export_home.take()
                && let Err(error) = switch_project(studio, &home)
            {
                studio.notify(&error, true);
            }
            if let Some(id) = self.export_back.take()
                && studio.project().timelines.iter().any(|t| t.id == id)
            {
                studio.apply(concat_project::Command::SelectTimeline { timeline_id: id });
            }
            if written {
                studio.notify(&t("generate.exportedAll"), false);
            }
            return;
        }
        let Some(dir) = self.export_dir.clone() else {
            return;
        };
        let name = match self.exports.remove(0) {
            Exporting::Timeline(id, name) => {
                // The video's own timelines come first, in its project.
                if let Some(home) = self.export_home.clone()
                    && let Err(error) = switch_project(studio, &home)
                {
                    studio.notify(&error, true);
                    self.exports.clear();
                    self.export_next(studio);
                    return;
                }
                // A timeline taken away since: passed over, not the open
                // one exported under its name.
                if !studio.project().timelines.iter().any(|t| t.id == id) {
                    self.export_next(studio);
                    return;
                }
                studio.apply(concat_project::Command::SelectTimeline { timeline_id: id });
                name
            }
            Exporting::Short(project, name) => {
                // The Short's project, open for its one timeline to be
                // written; one gone or empty is passed over.
                if let Err(error) = switch_project(studio, &project) {
                    log::warn!("generate: {name} not exported: {error}");
                    self.export_next(studio);
                    return;
                }
                if studio.timeline().clips.is_empty() {
                    self.export_next(studio);
                    return;
                }
                name
            }
        };
        export(dir, &name);
        self.export_started = true;
        wait();
    }

    /// Keeps what goes up with each Short beside the video: its title, its
    /// hook and its hashtags - the video's tags, and #Shorts - in
    /// `shorts.txt`, one after another, added to what is already there.
    fn keep_shorts_text<'a>(&self, studio: &Studio, boards: impl Iterator<Item = &'a Storyboard>) {
        let Some(job) = self.job(studio) else { return };
        let tags = self
            .publishing
            .as_ref()
            .map(|publishing| hashtags(&publishing.tags))
            .unwrap_or_default();
        let mut text = std::fs::read_to_string(job.dir.join("shorts.txt")).unwrap_or_default();
        for board in boards {
            let hook = board
                .scenes()
                .next()
                .and_then(|scene| scene.on_screen_text.clone())
                .unwrap_or_default();
            // Kept already when a line of the file is its title.
            if text.lines().any(|line| line == board.title.trim()) {
                continue;
            }
            text += &format!(
                "{}\n{}\n{} #Shorts\n\n",
                board.title.trim(),
                hook.trim(),
                tags
            );
        }
        let _ = std::fs::create_dir_all(&job.dir);
        if let Err(error) = std::fs::write(job.dir.join("shorts.txt"), text) {
            log::warn!("generate: the Shorts' text: {error}");
        }
    }

    /// Gives the Shorts kept before the video's tags were written the
    /// hashtags they lacked: each line that is #Shorts alone.
    fn refresh_shorts_tags(&self, studio: &Studio, tags: &[String]) {
        let Some(job) = self.job(studio) else { return };
        let file = job.dir.join("shorts.txt");
        let (Ok(text), tags) = (std::fs::read_to_string(&file), hashtags(tags)) else {
            return;
        };
        if tags.is_empty() {
            return;
        }
        let mended: Vec<String> = text
            .lines()
            .map(|line| match line.trim() {
                "#Shorts" => format!("{tags} #Shorts"),
                _ => line.to_owned(),
            })
            .collect();
        if let Err(error) = std::fs::write(&file, mended.join("\n") + "\n") {
            log::warn!("generate: the Shorts' text: {error}");
        }
    }

    fn research_first(&mut self, studio: &mut Studio) {
        let (key, model) = studio.prefs.generate.writer();
        if self.topic.trim().is_empty() {
            self.message = t("generate.describeIdeaFirst");
            return;
        }
        if key.trim().is_empty() {
            self.message = need_writer_key(&studio.prefs.generate);
            return;
        }
        let Some(job) = self.job(studio) else { return };
        self.remember(studio, &job);
        let brief = self.brief(studio);
        self.begin(t("generate.researching"));
        self.phase = 0;
        spawn_in_project(
            move || {
                let found = writer::research(&brief, &key, &model)?;
                job.keep_research(&found)?;
                Ok(found)
            },
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Researched(Box::new(result))))
            },
        );
    }

    fn suggest_ideas(&mut self, studio: &mut Studio) {
        let (key, model) = studio.prefs.generate.writer();
        if key.trim().is_empty() {
            self.message = need_writer_key(&studio.prefs.generate);
            return;
        }
        let brief = self.brief(studio);
        let field = self.topic.trim().to_owned();
        let made: Vec<String> = self.ideas.iter().map(|idea| idea.title.clone()).collect();
        self.begin(t("generate.ideasBusy"));
        self.phase = 0;
        spawn_in_project(
            move || writer::ideas(&field, &brief, &made, 5, &key, &model),
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Ideas(Box::new(result))))
            },
        );
    }

    /// Asks the writer, on a worker, for niches for a new channel: for
    /// what the person cares about, in the narration's language.
    fn find_niches(&mut self, studio: &mut Studio) {
        let (key, model) = studio.prefs.generate.writer();
        if key.trim().is_empty() {
            self.message = need_writer_key(&studio.prefs.generate);
            return;
        }
        let interests = self.niche_interests.trim().to_owned();
        let language = LANGUAGES[studio.prefs.generate.language.min(LANGUAGES.len() - 1)];
        self.begin(t("generate.nichesBusy"));
        self.phase = 0;
        spawn_in_project(
            move || niche::find(&interests, language, &key, &model),
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Niches(Box::new(result))))
            },
        );
    }

    /// Makes niche `index` a channel of its own: the choices as they are,
    /// under the niche's name, with its audience, voice, subjects and
    /// rules - saved among the channels and picked, its first videos the
    /// ideas to make. A channel of that name already saved is replaced.
    fn choose_niche(&mut self, studio: &mut Studio, index: usize) {
        let Some(chosen) = self.niches.get(index).cloned() else {
            return;
        };
        let prefs = &mut studio.prefs.generate;
        let mut channel = prefs.as_template(&chosen.name);
        let new = &mut channel.prefs;
        new.channel_name = chosen.name.clone();
        new.channel_audience = chosen.audience.clone();
        new.channel_voice = chosen.voice.clone();
        new.channel_topics = chosen.topics.clone();
        new.channel_rules = chosen.rules.clone();
        // The old channel's own lines are not this one's.
        new.channel_opening.clear();
        new.channel_closing.clear();
        new.channel_example.clear();
        prefs.keep_template(channel);
        if let Some(row) = prefs
            .templates
            .iter()
            .position(|kept| kept.name == chosen.name)
        {
            prefs.apply_template(row);
        }
        studio.prefs.save(&studio.host.dirs);
        self.ideas = chosen
            .first_videos
            .iter()
            .map(|title| VideoIdea {
                title: title.clone(),
                angle: String::new(),
            })
            .collect();
        self.niches.clear();
        self.niche_open = false;
        studio.notify(&tf("generate.nicheChosen", &[&chosen.name]), false);
        self.load_look_pictures(studio);
        self.tally(studio);
    }

    /// Designs the channel's look on a worker - colours, tagline, page
    /// description, thumbnail style - and draws its logo and banner with
    /// the picture maker chosen, into the channel's own folder.
    fn design_look(&mut self, studio: &mut Studio) {
        let prefs = studio.prefs.generate.clone();
        let (key, model) = prefs.writer();
        if key.trim().is_empty() {
            self.message = need_writer_key(&prefs);
            return;
        }
        let channel = channel_text(&prefs);
        let language = LANGUAGES[prefs.language.min(LANGUAGES.len() - 1)];
        let dir = identity::folder(&studio.host.dirs.data, &prefs.channel_name);
        let pictures = match shots_of(&prefs) {
            Shots::Models { pictures, .. } => Some(pictures),
            _ => None,
        };
        self.begin(t("generate.lookBusy"));
        self.phase = 0;
        spawn_in_project(
            move || {
                let look = identity::design(&channel, language, &key, &model)?;
                let drawn = match &pictures {
                    Some(pictures) => identity::draw(&look, pictures, &dir),
                    None => Err(t("generate.lookNoPictures")),
                };
                Ok((look, drawn))
            },
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Looked(Box::new(result))))
            },
        );
    }

    /// Plans the channel's next thirty days on a worker: as many long
    /// videos as the week's number makes, none made before.
    fn plan_month(&mut self, studio: &mut Studio) {
        let prefs = studio.prefs.generate.clone();
        let (key, model) = prefs.writer();
        if key.trim().is_empty() {
            self.message = need_writer_key(&prefs);
            return;
        }
        let channel = channel_text(&prefs);
        let language = LANGUAGES[prefs.language.min(LANGUAGES.len() - 1)];
        let made: Vec<String> = prefs.made_before().to_vec();
        let per_week = calendar::PER_WEEK[per_week_row(&prefs)];
        self.begin(t("generate.planBusy"));
        self.phase = 0;
        spawn_in_project(
            move || calendar::plan(&channel, language, &made, per_week, &key, &model),
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Planned(Box::new(result))))
            },
        );
    }

    /// Makes every video of the channel's plan not made yet, one after
    /// another in its order, as Make every idea makes a batch: each looked
    /// over as it is cut, held back from export when something is wrong,
    /// and marked made in the plan as its script is written. Cancel stops
    /// the one under way; Create carries it on where it stopped.
    fn make_plan(&mut self, studio: &mut Studio) {
        let left: Vec<String> = self
            .calendar
            .iter()
            .flat_map(|calendar| &calendar.entries)
            .filter(|entry| !entry.made)
            .map(|entry| entry.title.clone())
            .collect();
        if left.is_empty() {
            studio.notify(&t("generate.planAllMade"), false);
            return;
        }
        let one = self.estimate(studio).total();
        studio.notify(
            &tf(
                "generate.planMaking",
                &[&left.len(), &dollars(one * left.len() as f64)],
            ),
            false,
        );
        self.queue = left;
        self.next(studio);
    }

    /// Signs in to YouTube: Google's page opened in the browser, and a
    /// worker waiting on this device for it to come back, then trading
    /// its code for a refresh token and reading the channel's title.
    fn connect_youtube(&mut self, studio: &mut Studio) {
        let client = youtube_client(&studio.prefs.generate);
        let sign_in = match youtube::begin(&client) {
            Ok(sign_in) => sign_in,
            Err(error) => {
                self.message = error;
                return;
            }
        };
        if let Err(error) = crate::platform::open_web(&sign_in.url) {
            self.message = error;
            return;
        }
        self.begin(t("generate.youtubeSigningIn"));
        self.phase = 0;
        spawn(
            move || {
                let token = sign_in.finish(&client)?;
                let access = youtube::access(&client, &token)?;
                let channel = youtube::channel_title(&access)?;
                Ok((token, channel))
            },
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::YouTubeConnected(Box::new(
                    result,
                ))))
            },
        );
    }

    /// Puts the last video exported up on YouTube, on a worker: as a Short
    /// when it is one of the video's Shorts, by its file's name, with the
    /// words written for it; as the long video otherwise.
    fn upload_last(&mut self, studio: &mut Studio) {
        if self.uploading.is_some() {
            return;
        }
        let file = PathBuf::from(&studio.export.written);
        if studio.export.written.trim().is_empty() || !file.is_file() {
            self.message = t("generate.exportFirst");
            return;
        }
        let prefs = &studio.prefs.generate;
        let client = youtube_client(prefs);
        let token = prefs.youtube_token.clone();
        let privacy = youtube::Privacy::of_row(prefs.youtube_privacy);
        let stem = file
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tags = self
            .publishing
            .as_ref()
            .map(|publishing| publishing.tags.clone())
            .unwrap_or_default();
        let short = self
            .plan
            .iter()
            .flat_map(|plan| &plan.shorts)
            .find(|short| file_stem(&short.title) == stem);
        let upload = match (short, &self.publishing) {
            (Some(short), _) => youtube::Upload {
                title: short.title.clone(),
                description: short.pick.hook.clone(),
                tags,
                privacy,
                short: true,
            },
            (None, Some(publishing)) => youtube::Upload {
                title: publishing.title.clone(),
                description: publishing.full_description(),
                tags,
                privacy,
                short: false,
            },
            (None, None) => youtube::Upload {
                title: stem.clone(),
                privacy,
                ..youtube::Upload::default()
            },
        };
        let title = upload.title.clone();
        self.uploading = Some(0.0);
        studio.notify(&tf("generate.uploading", &[&title]), false);
        spawn(
            move || {
                let access = youtube::access(&client, &token)?;
                let id = youtube::upload(&access, &file, &upload, &mut |share| {
                    on_ui(move |studio, _, _| {
                        studio.handle(Msg::Generate(GenerateMsg::UploadProgress(share)))
                    });
                })?;
                Ok((id, title))
            },
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Uploaded(Box::new(result))))
            },
        );
    }

    /// Reads the numbers of every video the channel put up from here.
    fn read_numbers(&mut self, studio: &mut Studio) {
        let prefs = &studio.prefs.generate;
        let client = youtube_client(prefs);
        let token = prefs.youtube_token.clone();
        let dir = identity::folder(&studio.host.dirs.data, &prefs.channel_name);
        let ids: Vec<String> = read_uploads(&dir).into_iter().map(|(id, _)| id).collect();
        self.begin(t("generate.readingNumbers"));
        self.phase = 0;
        spawn(
            move || {
                if ids.is_empty() {
                    return Ok(Vec::new());
                }
                let access = youtube::access(&client, &token)?;
                youtube::numbers(&access, &ids)
            },
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::YouTubeNumbers(Box::new(result))))
            },
        );
    }

    /// What the plan of the week's number would cost, at the sheet's
    /// choices as they are now: the videos, one's cost, and all of them.
    fn plan_cost(&self, studio: &Studio) -> String {
        let prefs = &studio.prefs.generate;
        let count = calendar::Calendar::count_for(calendar::PER_WEEK[per_week_row(prefs)]);
        let one = self.estimate(studio).total();
        tf(
            "generate.planCost",
            &[&count, &dollars(one), &dollars(one * count as f64)],
        )
    }

    /// Keeps the channel's look: its colours and words in the channel's
    /// choices, its logo in the corner of every video and its banner, and
    /// in the channel saved under its name when there is one.
    fn take_look(
        &mut self,
        studio: &mut Studio,
        look: identity::Identity,
        drawn: Result<(PathBuf, PathBuf), String>,
    ) {
        let prefs = &mut studio.prefs.generate;
        prefs.brand_color = look.colour.clone();
        prefs.brand_second = look.second.clone();
        prefs.channel_tagline = look.tagline.clone();
        prefs.channel_about = look.about.clone();
        prefs.thumbnail_style = look.thumbnail_style.clone();
        match drawn {
            Ok((logo, banner)) => {
                prefs.brand_logo = logo.to_string_lossy().into_owned();
                prefs.brand_banner = banner.to_string_lossy().into_owned();
                studio.notify(&t("generate.lookReady"), false);
            }
            Err(why) => {
                log::warn!("generate: the channel's pictures: {why}");
                studio.notify(&tf("generate.lookUndrawn", &[&why]), true);
            }
        }
        let prefs = &mut studio.prefs.generate;
        let name = prefs.channel_name.trim().to_owned();
        if !name.is_empty() && prefs.templates.iter().any(|kept| kept.name == name) {
            let channel = prefs.as_template(&name);
            prefs.keep_template(channel);
        }
        studio.prefs.save(&studio.host.dirs);
        self.load_look_pictures(studio);
    }

    /// Loads the channel's logo and banner for the sheet to show.
    fn load_look_pictures(&mut self, studio: &Studio) {
        let load = |path: &str| {
            Some(path)
                .filter(|path| !path.trim().is_empty())
                .and_then(|path| image_at(Path::new(path)))
                .unwrap_or_default()
        };
        let prefs = &studio.prefs.generate;
        self.look_pictures = (load(&prefs.brand_logo), load(&prefs.brand_banner));
        // The channel's plan is kept beside its look.
        self.calendar = (!prefs.channel_name.trim().is_empty())
            .then(|| identity::folder(&studio.host.dirs.data, &prefs.channel_name))
            .and_then(|dir| calendar::Calendar::read(&dir));
    }

    /// The niches proposed, as the sheet lists them.
    fn niche_rows(&self) -> Vec<NicheRow> {
        self.niches
            .iter()
            .map(|found| NicheRow {
                name: found.name.as_str().into(),
                summary: found.summary.as_str().into(),
                why: found.why.as_str().into(),
                scores: tf(
                    "generate.nicheScores",
                    &[
                        &found.demand,
                        &found.competition,
                        &found.revenue,
                        &found.faceless,
                    ],
                )
                .into(),
                videos: found
                    .first_videos
                    .iter()
                    .map(|title| format!("• {title}"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into(),
            })
            .collect()
    }

    /// The ideas list's rows: what to do, then each idea and why.
    pub fn idea_rows(&self) -> Vec<SharedString> {
        std::iter::once(t("generate.ideasPick"))
            .chain(self.ideas.iter().map(|idea| match idea.angle.trim() {
                "" => idea.title.clone(),
                angle => format!("{} — {angle}", idea.title),
            }))
            .map(SharedString::from)
            .collect()
    }

    /// The channels' list: what to do, then each series saved, then the
    /// ready channel while none is saved under its name.
    pub fn template_rows(&self, studio: &Studio) -> Vec<SharedString> {
        let prefs = &studio.prefs.generate;
        std::iter::once(t("generate.noTemplate"))
            .chain(prefs.templates.iter().map(|template| template.name.clone()))
            .chain((!has_la_logica(prefs)).then(|| {
                format!(
                    "{} · {}",
                    crate::prefs::LA_LOGICA,
                    t("generate.readyChannel")
                )
            }))
            .map(SharedString::from)
            .collect()
    }

    /// Starts the next idea of a batch, once the run before it is over and
    /// its export is; looks again in a moment while it is not.
    fn next(&mut self, studio: &mut Studio) {
        if self.queue.is_empty() {
            return;
        }
        let settling = self
            .ended
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(8));
        if self.running || settling || studio.export.phase == crate::ui::ExportPhase::Running {
            spawn_in_project(
                || std::thread::sleep(std::time::Duration::from_secs(3)),
                |studio, _, _, ()| studio.handle(Msg::Generate(GenerateMsg::Next)),
            );
            return;
        }
        let topic = self.queue.remove(0);
        studio.notify(
            &tf("generate.batchNext", &[&topic, &self.queue.len()]),
            false,
        );
        self.topic = topic;
        self.source.clear();
        self.look(studio);
        // Made as Create makes it; the one after it is looked for once
        // this one is under way.
        on_ui(|studio, _, _| {
            studio.handle(Msg::Generate(GenerateMsg::Create));
            studio.handle(Msg::Generate(GenerateMsg::Next));
        });
    }

    /// Makes the video again in another language: its script translated,
    /// its pictures taken over as they are, then its voice read and its
    /// drawn shots drawn again, as a video of its own.
    fn translate(&mut self, studio: &mut Studio) {
        let (Some(board), Some(from)) = (self.board.clone(), self.job(studio)) else {
            return;
        };
        let row = self.also_language.min(LANGUAGES.len() - 1);
        let language = LANGUAGES[row];
        if board.language.trim().eq_ignore_ascii_case(language) {
            self.message = t("generate.translateSame");
            return;
        }
        let (key, model) = studio.prefs.generate.writer();
        if key.trim().is_empty() {
            self.message = need_writer_key(&studio.prefs.generate);
            return;
        }
        let Some(project) = studio
            .session
            .as_ref()
            .map(|session| session.path().to_owned())
        else {
            return;
        };
        let topic = format!("{} ({language})", self.topic.trim());
        self.begin(tf("generate.translating", &[&1, &board.chapters.len()]));
        self.phase = 0;
        spawn_in_project(
            move || {
                let translated =
                    writer::translate(&board, language, &key, &model, &mut |n, of| {
                        let stage = tf("generate.translating", &[&n, &of]);
                        let fraction = n as f32 / (of + 1) as f32;
                        on_ui(move |studio, _, _| {
                            studio.handle(Msg::Generate(GenerateMsg::Progress(
                                fraction,
                                stage,
                                String::new(),
                            )));
                        });
                    })?;
                let job = Job::new(Path::new(&project), &topic);
                job.adopt(&from)?;
                job.keep(&translated)?;
                Ok((topic, translated, row))
            },
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::Translated(Box::new(result))))
            },
        );
    }

    /// Picks the video's Shorts for the results - [`shorts::PLANNED`]
    /// stretches, each a different idea of it - on a worker, and keeps
    /// them with the video. Nothing is made: each Short is made when asked.
    fn plan_shorts(&mut self, studio: &mut Studio) {
        let (Some(board), Some(job)) = (self.board.clone(), self.job(studio)) else {
            return;
        };
        let (key, model) = studio.prefs.generate.writer();
        let brief = self.brief(studio);
        self.begin(t("generate.pickingShort"));
        self.phase = 3;
        spawn_in_project(
            move || {
                let board = voice_timed(&job, &board);
                let picks = writer::pick_shorts(&board, &brief, shorts::PLANNED, &key, &model)
                    .unwrap_or_else(|error| {
                        // As many all the same, from the start of the
                        // video on, when the writer cannot be asked.
                        log::warn!("generate: the Shorts' stretches: {error}");
                        shorts::filled(&board, Vec::new(), shorts::PLANNED)
                    });
                Ok(shorts::Plan::of(&board, picks))
            },
            |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::ShortsPlanned(Box::new(result))))
            },
        );
    }

    /// Gathers what Short `index` of the results is cut from, on a worker;
    /// nothing is made or paid for again. It is laid down in a project of
    /// its own, beside the video's, when it comes.
    fn make_short(&mut self, studio: &mut Studio, index: usize) {
        if self.running {
            return;
        }
        let (Some(board), Some(job)) = (self.board.clone(), self.job(studio)) else {
            return;
        };
        let Some(planned) = self
            .plan
            .as_ref()
            .and_then(|plan| plan.shorts.get(index))
            .cloned()
        else {
            return;
        };
        self.begin(tf("generate.makingShort", &[&planned.title]));
        self.phase = 3;
        self.making_short = Some(index);
        spawn_in_project(
            move || {
                let board = voice_timed(&job, &board);
                let mut short = shorts::board(&board, &planned.pick);
                short.title = planned.title.clone();
                let made = job.gather(&short, true)?;
                Ok((short, made))
            },
            move |studio, _, _, result| {
                studio.handle(Msg::Generate(GenerateMsg::ShortMade(
                    index,
                    Box::new(result),
                )))
            },
        );
    }

    /// Lays Short `index` down in a project of its own beside the video's,
    /// leaving the window on the video: the project is on the recents list
    /// to open, its captions and what goes up with it in the video's
    /// folder.
    fn short_project(
        &mut self,
        studio: &mut Studio,
        index: usize,
        board: &Storyboard,
        made: &Made,
    ) {
        let (Some(job), Some(beside)) = (
            self.job(studio),
            studio
                .session
                .as_ref()
                .map(|session| PathBuf::from(session.path())),
        ) else {
            return;
        };
        let Some(Prepared {
            placements,
            media,
            options,
            captioned,
        }) = self.prepare_cut(studio, board, made, true)
        else {
            return;
        };
        let config = studio.host.dirs.config.clone();
        let made_into = project_apart(&beside, &config, &board.title, board.frame(), |apart| {
            let (_, left_out) = plan::cut(apart, board, &placements, &media, &options)?;
            for path in &left_out {
                log::warn!("generate: {path} was left out of the Short");
            }
            Ok(())
        });
        match made_into {
            Ok(project) => {
                if let Some(plan) = self.plan.as_mut()
                    && let Some(short) = plan.shorts.get_mut(index)
                {
                    short.project = project;
                    if let Err(error) = plan.keep(&job.dir) {
                        log::warn!("generate: the Shorts' plan: {error}");
                    }
                }
                if let Some(captioned) = &captioned {
                    keep_captions(&job.dir, &board.title, captioned);
                }
                self.keep_shorts_text(studio, std::iter::once(board));
                studio.notify(&tf("generate.shortSaved", &[&board.title]), false);
            }
            Err(error) => self.failed(error),
        }
    }

    /// What a cut of `board` lays down from what was `made`: its plan,
    /// the files it imports, its options, and the cards a captions file
    /// is written from. `None`, and the sheet says why, when it cannot be
    /// planned.
    fn prepare_cut(
        &mut self,
        studio: &mut Studio,
        board: &Storyboard,
        made: &Made,
        short: bool,
    ) -> Option<Prepared> {
        let music = match self.music {
            AI_MUSIC => made.music.clone(),
            row => row.checked_sub(AI_MUSIC + 1).and_then(|row| {
                music_items(studio)
                    .get(row)
                    .map(|(path, length)| (path.clone(), *length))
            }),
        };
        let mut media = made.media.clone();
        if let Some((path, _)) = &music
            && let Ok(summary) = concat_host::media::probe(path)
        {
            media.insert(path.clone(), summary.to_new_media());
        }
        // The channel's cards where it wants them and they were drawn,
        // and its logo where the picture can be read.
        let bumpers = made
            .bumpers
            .clone()
            // Drawn for the video's frame: a Short's is another shape.
            .filter(|_| !short && brand_of(&studio.prefs.generate).is_some());
        let logo = Some(studio.prefs.generate.brand_logo.clone())
            .filter(|path| !path.trim().is_empty())
            .filter(|path| {
                concat_host::media::probe(path)
                    .map(|summary| {
                        media.insert(path.clone(), summary.to_new_media());
                    })
                    .inspect_err(|error| log::warn!("generate: the logo: {error}"))
                    .is_ok()
            });
        // The cut's own sounds, made in the job's folder and probed like
        // any other file; a cut without them where they cannot be made.
        let sounds = self
            .job(studio)
            .filter(|_| studio.prefs.generate.sounds.unwrap_or(true))
            .and_then(|job| {
                concat_generate::sfx::sounds(&concat_generate::sfx::dir_in(&job.dir))
                    .inspect_err(|error| log::warn!("generate: the sounds: {error}"))
                    .ok()
            })
            .filter(|sounds| {
                sounds.all().iter().all(|sound| {
                    concat_host::media::probe(&sound.path)
                        .map(|summary| {
                            media.insert(sound.path.clone(), summary.to_new_media());
                        })
                        .is_ok()
                })
            });
        let options = Options {
            // A Short is captioned, a few words at a time, and its wide
            // pictures fill the tall frame.
            captions: self.captions || short,
            titles: self.titles || short,
            caption_look: match studio.prefs.generate.caption_look {
                _ if short => CaptionLook::Animated,
                1 => CaptionLook::Animated,
                _ => CaptionLook::Classic,
            },
            fill_frame: short,
            sounds,
            intro: bumpers.as_ref().map(|(intro, _)| intro.clone()),
            outro: bumpers.as_ref().map(|(_, outro)| outro.clone()),
            logo,
            accent: Some(studio.prefs.generate.brand_color.trim().to_owned())
                .filter(|colour| !colour.is_empty()),
            // Something new every few seconds where the look is short-form.
            hold: if short {
                2.5
            } else if studio.prefs.generate.caption_look == 1 {
                3.0
            } else {
                0.0
            },
            ..Options::default()
        };
        let placements = match plan::plan(
            board,
            &made.files,
            &options,
            music
                .as_ref()
                .map(|(path, length)| (path.as_str(), *length)),
        ) {
            Ok(placements) => placements,
            Err(error) => {
                self.message = error;
                return None;
            }
        };
        // The captions as a file of their own, for an upload page to take
        // beside the video: the cards the cut shows, or would show when
        // they are not burnt in.
        let captioned = if options.captions {
            Some(placements.clone())
        } else {
            plan::plan(
                board,
                &made.files,
                &Options {
                    captions: true,
                    ..options.clone()
                },
                music
                    .as_ref()
                    .map(|(path, length)| (path.as_str(), *length)),
            )
            .inspect_err(|error| log::warn!("generate: the captions file: {error}"))
            .ok()
        };
        Some(Prepared {
            placements,
            media,
            options,
            captioned,
        })
    }

    /// Lays the cut on a new timeline of the window's project, as one
    /// undo step, and closes.
    fn cut(&mut self, studio: &mut Studio, board: &Storyboard, made: &Made, short: bool) {
        let Some(Prepared {
            placements,
            media,
            options,
            captioned,
        }) = self.prepare_cut(studio, board, made, short)
        else {
            return;
        };
        // One batch: the bookkeeping after each of the cut's clips is done
        // once, at the end.
        let laid = studio
            .batch(|studio| plan::cut(&mut Session(studio), board, &placements, &media, &options));
        match laid {
            Ok((_, left_out)) => {
                self.open = false;
                let minutes = plan::total_length(&placements) / 60.0;
                studio.notify(
                    &tf("generate.videoReady", &[&format!("{minutes:.1}")]),
                    false,
                );
                if made.spent > 0.0 {
                    studio.notify(&tf("generate.spent", &[&dollars(made.spent)]), false);
                }
                if let (Some(captioned), Some(job)) = (&captioned, self.job(studio)) {
                    keep_captions(&job.dir, &board.title, captioned);
                }
                for stand_in in &made.stand_ins {
                    log::warn!("generate: {stand_in}");
                }
                if !made.stand_ins.is_empty() {
                    studio.notify(&tf("generate.standIns", &[&made.stand_ins.len()]), true);
                }
                if !left_out.is_empty() {
                    let names: Vec<String> = left_out
                        .iter()
                        .map(|path| {
                            Path::new(path)
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.clone())
                        })
                        .collect();
                    studio.notify(&tf("generate.leftOut", &[&names.join(", ")]), true);
                }
                // Looked over before it goes out. A Short is cut from a
                // video already looked over.
                let findings = if short {
                    Vec::new()
                } else {
                    review::review(
                        board,
                        &made.files,
                        &made.stand_ins,
                        &placements,
                        left_out.len(),
                        self.voice_row(studio) != 0,
                    )
                };
                let said: Vec<String> = findings.iter().map(finding_text).collect();
                for line in &said {
                    log::info!("generate: review: {line}");
                }
                if findings.iter().any(review::Finding::holds) {
                    // Something is missing from it: shown, and not
                    // exported until the person says so.
                    self.open = true;
                    self.message = format!("{}\n{}", t("generate.reviewHeld"), said.join("\n"));
                    studio.notify(&t("generate.reviewHeld"), true);
                    return;
                }
                if !short {
                    match said.is_empty() {
                        true => studio.notify(&t("generate.reviewClean"), false),
                        false => studio.notify(&said.join(" "), false),
                    }
                }
                if studio.prefs.generate.auto_export.unwrap_or(true)
                    && let Some(job) = self.job(studio)
                {
                    export(job.dir, &board.title);
                }
            }
            Err(error) => self.message = error,
        }
    }

    /// Clears the last run's words and starts the bar.
    fn begin(&mut self, stage: String) {
        self.stop.store(false, Ordering::Release);
        self.running = true;
        self.message.clear();
        self.progress = 0.0;
        self.stage = stage;
        self.detail.clear();
        self.preview = None;
        self.counts.clear();
        self.spent = 0.0;
    }

    /// Who reads: the remembered choice, or the window's own voices when a
    /// speech model is installed and nothing has been chosen.
    fn voice_row(&self, studio: &Studio) -> usize {
        studio.prefs.generate.voice.unwrap_or_else(|| {
            if installed(&studio.settings.voices).is_empty() {
                0
            } else {
                1
            }
        })
    }

    /// The logo list's rows: none, then every picture it may be.
    pub fn logo_rows(&self, studio: &Studio) -> Vec<SharedString> {
        std::iter::once(t("generate.noLogo"))
            .chain(logo_items(studio).into_iter().map(|path| {
                Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or(path)
            }))
            .map(SharedString::from)
            .collect()
    }

    /// The music list's rows: none, music made for the video, then every
    /// recording in the bin.
    pub fn music_rows(&self, studio: &Studio) -> Vec<SharedString> {
        [t("generate.noMusic"), t("generate.aiMusic")]
            .into_iter()
            .chain(music_items(studio).into_iter().map(|(path, _)| {
                Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or(path)
            }))
            .map(SharedString::from)
            .collect()
    }

    /// The sheet as Slint shows it.
    pub fn data(&self, studio: &Studio) -> GenerateSheetData {
        let prefs = &studio.prefs.generate;
        // A run on this job started before the person last left the
        // project: shown going on, read from where every run says how far
        // it is, and not started again.
        let away = (!self.running)
            .then(|| self.job(studio))
            .flatten()
            .and_then(|job| crate::runs::of(&job.dir));
        let summary = self.board.as_ref().map(|board| {
            // A tenth of a minute while that is a difference worth seeing:
            // a short draft would otherwise read as "about 0 min".
            let minutes = board.seconds() / 60.0;
            let minutes = if minutes < 10.0 {
                format!("{minutes:.1}")
            } else {
                format!("{minutes:.0}")
            };
            let (clips, stills) = board.kinds();
            tf(
                "generate.scriptReady",
                &[
                    &board.title,
                    &board.scenes().count(),
                    &clips,
                    &stills,
                    &minutes,
                ],
            )
        });
        let totals = budget::budget(&self.brief(studio).pacing()).totals();
        GenerateSheetData {
            open: self.open,
            topic: self.topic.as_str().into(),
            notes: self.notes.as_str().into(),
            minutes: self.minutes as i32,
            clips: self.clips_row(studio) as i32,
            budget: tf(
                "generate.budget",
                &[
                    &totals.chapters,
                    &totals.scenes,
                    &totals.clips,
                    &totals.stills,
                    &totals.words,
                ],
            )
            .into(),
            cost: self.cost_line(studio).into(),
            spend_limit: prefs.spend_limit.as_str().into(),
            clip_price: prefs.clip_price.as_str().into(),
            image_price: prefs.image_price.as_str().into(),
            language: prefs.language as i32,
            aspect: prefs.aspect as i32,
            voice: self.voice_row(studio) as i32,
            shots: prefs.shots as i32,
            music: self.music as i32,
            captions: self.captions,
            caption_look: prefs.caption_look as i32,
            content_style: prefs.content_style as i32,
            level: level_row(prefs) as i32,
            research: prefs.research.unwrap_or(true),
            visuals: prefs.visuals.min(1) as i32,
            picture_style: prefs.pictures.min(PictureStyle::ALL.len() - 1) as i32,
            picture_quality: match prefs.replicate_image_model.trim() {
                PROFESSIONAL_IMAGE_MODEL => 1,
                BEST_IMAGE_MODEL => 2,
                _ => 0,
            },
            channel_name: prefs.channel_name.as_str().into(),
            channel_audience: prefs.channel_audience.as_str().into(),
            channel_voice: prefs.channel_voice.as_str().into(),
            channel_opening: prefs.channel_opening.as_str().into(),
            channel_closing: prefs.channel_closing.as_str().into(),
            channel_example: prefs.channel_example.as_str().into(),
            titles: self.titles,
            anthropic_key: prefs.anthropic_key.as_str().into(),
            writer: prefs.writer as i32,
            openai_key: prefs.openai_key.as_str().into(),
            writer_model: prefs.writer_model.as_str().into(),
            writer_model_default: concat_generate::chatgpt::DEFAULT_MODEL.into(),
            replicate_token: prefs.replicate_token.as_str().into(),
            replicate_model: prefs.replicate_model.as_str().into(),
            replicate_image_model: prefs.replicate_image_model.as_str().into(),
            picture_maker: prefs.picture_maker.min(2) as i32,
            clip_maker: prefs.clip_maker.min(2) as i32,
            picture_key: match prefs.picture_maker {
                1 => prefs.openai_key.as_str(),
                _ => prefs.gemini_key.as_str(),
            }
            .into(),
            picture_model: match prefs.picture_maker {
                1 => prefs.openai_image_model.as_str(),
                _ => prefs.gemini_image_model.as_str(),
            }
            .into(),
            picture_model_default: match prefs.picture_maker {
                1 => makers::OPENAI_IMAGE_MODEL,
                _ => makers::GEMINI_IMAGE_MODEL,
            }
            .into(),
            picture_models: std::rc::Rc::new(slint::VecModel::from(
                self.picture_models
                    .names
                    .iter()
                    .map(|name| SharedString::from(name.as_str()))
                    .collect::<Vec<_>>(),
            ))
            .into(),
            picture_model_index: {
                let chosen = match prefs.picture_maker {
                    1 => prefs.openai_image_model.trim(),
                    _ => prefs.gemini_image_model.trim(),
                };
                let chosen = match (chosen.is_empty(), prefs.picture_maker) {
                    (false, _) => chosen,
                    (true, 1) => makers::OPENAI_IMAGE_MODEL,
                    (true, _) => makers::GEMINI_IMAGE_MODEL,
                };
                self.picture_models
                    .names
                    .iter()
                    .position(|name| name == chosen)
                    .map_or(-1, |at| at as i32)
            },
            picture_models_note: if self.picture_models.asking {
                t("generate.askingPictureModels")
            } else if self.picture_models.error.is_empty() {
                String::new()
            } else {
                tf(
                    "generate.pictureModelsFailed",
                    &[&self.picture_models.error],
                )
            }
            .into(),
            clip_key: match prefs.clip_maker {
                1 => prefs.kling_access_key.as_str(),
                _ => prefs.seedance_key.as_str(),
            }
            .into(),
            clip_secret: prefs.kling_secret_key.as_str().into(),
            clip_model: match prefs.clip_maker {
                1 => prefs.kling_model.as_str(),
                _ => prefs.seedance_model.as_str(),
            }
            .into(),
            clip_model_default: match prefs.clip_maker {
                1 => makers::KLING_MODEL,
                _ => makers::SEEDANCE_MODEL,
            }
            .into(),
            auto_export: prefs.auto_export.unwrap_or(true),
            sounds: prefs.sounds.unwrap_or(true),
            format: prefs.format.min(VideoFormat::ALL.len() - 1) as i32,
            structure: prefs.structure.as_str().into(),
            channel_topics: prefs.channel_topics.as_str().into(),
            channel_rules: prefs.channel_rules.as_str().into(),
            shorts_count: prefs.shorts_count() as i32,
            channel_memory: match prefs.made_before().len() {
                0 => SharedString::new(),
                made => tf("generate.channelMemory", &[&made.to_string()]).into(),
            },
            channel_row: channel_row(prefs),
            publish_check: match &self.checked {
                None => SharedString::new(),
                Some(found) if found.is_empty() => t("generate.checkClean").into(),
                Some(found) => found
                    .iter()
                    .map(|line| format!("• {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into(),
            },
            research_notes: self
                .research
                .as_ref()
                .map(|research| research.notes.as_str())
                .unwrap_or_default()
                .into(),
            research_sources: match self.research.as_ref().map_or(0, |r| r.sources.len()) {
                0 => SharedString::new(),
                n => tf("generate.researchSources", &[&n]).into(),
            },
            has_research: self.research.is_some(),
            source: self.source.as_str().into(),
            also_language: self.also_language as i32,
            has_ideas: !self.ideas.is_empty(),
            queued: self.queue.len() as i32,
            brand_color: prefs.brand_color.as_str().into(),
            logo: logo_items(studio)
                .iter()
                .position(|path| *path == prefs.brand_logo)
                .map_or(0, |row| row as i32 + 1),
            bumpers: prefs.brand_bumpers,
            hook: prefs.hook.unwrap_or(true),
            deep: prefs.deep.unwrap_or(false),
            picture_check: prefs.picture_check.unwrap_or(false),
            elevenlabs_key: prefs.elevenlabs_key.as_str().into(),
            elevenlabs_voice: prefs.elevenlabs_voice.as_str().into(),
            voice_model: prefs.elevenlabs_model.min(1) as i32,
            pronunciation: prefs.pronunciation.as_str().into(),
            has_script: self.board.is_some(),
            script_summary: summary.unwrap_or_default().into(),
            status: self.status.as_str().into(),
            can_short: self.all_made && !self.running && away.is_none(),
            results: self.all_made && away.is_none(),
            niche_open: self.niche_open,
            look_words: [
                prefs.channel_tagline.trim(),
                prefs.channel_about.trim(),
                prefs.thumbnail_style.trim(),
            ]
            .iter()
            .filter(|words| !words.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n\n")
            .into(),
            look_colours: [prefs.brand_color.trim(), prefs.brand_second.trim()]
                .iter()
                .filter(|colour| !colour.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join(" · ")
                .into(),
            plan_per_week: per_week_row(prefs) as i32,
            youtube_client_id: prefs.youtube_client_id.as_str().into(),
            youtube_client_secret: prefs.youtube_client_secret.as_str().into(),
            youtube_channel: prefs.youtube_channel.as_str().into(),
            youtube_privacy: prefs.youtube_privacy.min(2) as i32,
            uploading: self.uploading.is_some(),
            upload_progress: self.uploading.unwrap_or(0.0),
            youtube_numbers: self.youtube_numbers.as_str().into(),
            plan_left: self
                .calendar
                .iter()
                .flat_map(|calendar| &calendar.entries)
                .filter(|entry| !entry.made)
                .count() as i32,
            plan_cost: self.plan_cost(studio).into(),
            plan: std::rc::Rc::new(slint::VecModel::from(
                self.calendar
                    .iter()
                    .flat_map(|calendar| &calendar.entries)
                    .map(|entry| PlanRow {
                        day: tf("generate.planDay", &[&entry.day]).into(),
                        title: entry.title.as_str().into(),
                        angle: entry.angle.as_str().into(),
                        made: entry.made,
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
            look_logo: self.look_pictures.0.clone(),
            look_banner: self.look_pictures.1.clone(),
            niche_interests: self.niche_interests.as_str().into(),
            niches: std::rc::Rc::new(slint::VecModel::from(self.niche_rows())).into(),
            shorts: std::rc::Rc::new(slint::VecModel::from(
                self.plan
                    .iter()
                    .filter(|_| self.all_made && away.is_none())
                    .flat_map(|plan| plan.shorts.iter().enumerate())
                    .map(|(n, short)| ShortResult {
                        title: short.title.as_str().into(),
                        state: if self.making_short == Some(n) {
                            1
                        } else if short.project.is_empty() {
                            0
                        } else {
                            2
                        },
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
            running: self.running || away.is_some(),
            progress: away.as_ref().map_or(self.progress, |run| run.progress),
            stage: away
                .as_ref()
                .map_or(self.stage.as_str().into(), |run| run.stage.as_str().into()),
            phase: self.phase,
            voiced: self.voiced,
            detail: self.detail.as_str().into(),
            has_preview: self.preview.is_some(),
            preview: self.preview.clone().unwrap_or_default(),
            counts: self.counts.as_str().into(),
            spent: if self.spent > 0.0 {
                tf("generate.spentSoFar", &[&dollars(self.spent)]).into()
            } else {
                SharedString::default()
            },
            message: self.message.as_str().into(),
            publishing: self.publishing_busy,
            has_publishing: self.publishing.is_some(),
            publish_title: self
                .publishing
                .as_ref()
                .map(|p| p.title.trim().to_owned())
                .unwrap_or_default()
                .into(),
            publish_description: self
                .publishing
                .as_ref()
                .map(Publishing::full_description)
                .unwrap_or_default()
                .into(),
            publish_tags: self
                .publishing
                .as_ref()
                .map(Publishing::tag_line)
                .unwrap_or_default()
                .into(),
        }
    }
}

impl GeneratePane {
    /// The script editor as Slint shows it.
    pub fn script_data(&self, studio: &Studio) -> ScriptSheetData {
        let Some(board) = self.board.as_ref().filter(|_| self.script_open) else {
            return ScriptSheetData::default();
        };
        let (clips, stills) = board.kinds();
        let minutes = board.seconds() / 60.0;
        let planned = budget::budget(&self.brief(studio).pacing()).totals();
        ScriptSheetData {
            open: true,
            summary: tf(
                "script.summary",
                &[
                    &board.title,
                    &format!("{minutes:.1}"),
                    &board.chapters.len(),
                    &board.scenes().count(),
                    &clips,
                    &stills,
                    &board.words(),
                ],
            )
            .into(),
            planned: tf(
                "script.planned",
                &[
                    &MINUTES[self.minutes.min(MINUTES.len() - 1)],
                    &planned.scenes,
                    &planned.clips,
                    &planned.stills,
                ],
            )
            .into(),
            warning: [
                (!self.repeats.is_empty()).then(|| tf("script.repeats", &[&self.repeats.len()])),
                (!self.lookalikes.is_empty())
                    .then(|| tf("script.lookalikes", &[&self.lookalikes.len()])),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .into(),
            message: self.script_message.as_str().into(),
        }
    }

    /// The script editor's rows: each chapter, then its scenes, each scene
    /// then its shots.
    pub fn script_rows(&self, studio: &Studio) -> Vec<ScriptRow> {
        let Some(board) = self.board.as_ref().filter(|_| self.script_open) else {
            return Vec::new();
        };
        let prices = prices(studio);
        let paid = studio.prefs.generate.shots == 1;
        let mut rows = Vec::new();
        // What deeper analysis worked out and found, first, to read.
        let editorial = board.editorial.as_ref();
        if let Some(editorial) = editorial {
            let analysis = analysis_text(&editorial.analysis);
            if !analysis.is_empty() {
                rows.push(ScriptRow {
                    kind: 3,
                    chapter: -1,
                    scene: -1,
                    shot: -1,
                    label: t("script.analysis").into(),
                    text: analysis.into(),
                    ..ScriptRow::default()
                });
            }
            if let Some(review) = &editorial.review {
                rows.push(ScriptRow {
                    kind: 3,
                    chapter: -1,
                    scene: -1,
                    shot: -1,
                    label: t("script.review").into(),
                    text: review_text(review).into(),
                    ..ScriptRow::default()
                });
            }
        }
        // What the review held against each scene, by its id.
        let notes = |id: &str| -> (String, bool) {
            let Some(review) = editorial.and_then(|editorial| editorial.review.as_ref()) else {
                return (String::new(), false);
            };
            let said: Vec<&str> = review
                .issues
                .iter()
                .filter(|issue| issue.scene == id)
                .map(|issue| issue.problem.trim())
                .filter(|problem| !problem.is_empty())
                .collect();
            (
                said.join(" "),
                review.revised.iter().any(|revised| revised == id),
            )
        };
        for (c, chapter) in board.chapters.iter().enumerate() {
            rows.push(ScriptRow {
                kind: 0,
                chapter: c as i32,
                scene: -1,
                shot: -1,
                label: tf("script.chapter", &[&(c + 1)]).into(),
                text: chapter.title.as_str().into(),
                detail: chapter.mood.as_str().into(),
                extra: chapter.summary.as_str().into(),
                ..ScriptRow::default()
            });
            for (s, scene) in chapter.scenes.iter().enumerate() {
                rows.push(ScriptRow {
                    kind: 1,
                    chapter: c as i32,
                    scene: s as i32,
                    shot: -1,
                    label: format!("{} · {:.0} s", scene.id, scene.seconds).into(),
                    text: scene.narration.as_str().into(),
                    detail: scene.voice.as_str().into(),
                    extra: scene.on_screen_text.as_deref().unwrap_or_default().into(),
                    busy: self.rewriting == Some((c, s)),
                    note: notes(&scene.id).0.into(),
                    revised: notes(&scene.id).1,
                    ..ScriptRow::default()
                });
                for (n, shot) in scene.shots.iter().enumerate() {
                    let stem = shot_stem(&scene.id, n);
                    rows.push(ScriptRow {
                        kind: 2,
                        chapter: c as i32,
                        scene: s as i32,
                        shot: n as i32,
                        label: format!(
                            "{} · {:.1} s",
                            tf("script.shot", &[&(n + 1)]),
                            shot.seconds
                        )
                        .into(),
                        text: match &shot.graphic {
                            Some(graphic) if shot.kind == ShotKind::Motion => {
                                graphic.title.as_str().into()
                            }
                            _ => shot.prompt.as_str().into(),
                        },
                        detail: match &shot.graphic {
                            Some(graphic) if shot.kind == ShotKind::Motion => {
                                card_lines(graphic).into()
                            }
                            _ => shot.motion.as_str().into(),
                        },
                        video: shot.kind == ShotKind::Video,
                        card: shot.kind == ShotKind::Motion,
                        explainer: shot.kind == ShotKind::Explainer,
                        repeat: self.repeats.contains(&stem),
                        lookalike: self.lookalikes.contains(&stem),
                        has_thumb: self.thumbs.contains_key(&stem),
                        thumb: self.thumbs.get(&stem).cloned().unwrap_or_default(),
                        remaking: self.remaking.contains(&stem),
                        suggesting: self.suggesting.contains(&(c, s, n)),
                        check_state: match self.picture_checks.get(&stem) {
                            None => 0,
                            Some(check) if !check.checked => 3,
                            Some(check) if check.is_acceptable => 1,
                            Some(_) => 2,
                        },
                        check: self
                            .picture_checks
                            .get(&stem)
                            .map(check_text)
                            .unwrap_or_default()
                            .into(),
                        cost: if paid {
                            let cost = match shot.kind {
                                ShotKind::Video => prices.clip + prices.still,
                                ShotKind::Image => prices.still,
                                ShotKind::Motion | ShotKind::Explainer => 0.0,
                            };
                            format!("~${}", dollars(cost)).into()
                        } else {
                            SharedString::default()
                        },
                        ..ScriptRow::default()
                    });
                }
            }
        }
        rows
    }
}

/// Renders the cut through the export sheet, exactly as pressing Export
/// would, into the job's folder under the video's title: the last step of
/// a run nobody has to watch. Posted for the next turn, so it starts once
/// the cut is published and the sheet's own state is its to change.
fn export(folder: PathBuf, title: &str) {
    let name = file_stem(title);
    let folder = folder.to_string_lossy().into_owned();
    on_ui(move |studio, _, _| {
        studio.handle(Msg::Export(ExportMsg::Open));
        studio.handle(Msg::Export(ExportMsg::NameEdited(name)));
        studio.handle(Msg::Export(ExportMsg::FolderChosen(folder)));
        // Overwrite: a run made again replaces its own file, not asks.
        studio.handle(Msg::Export(ExportMsg::Overwrite));
    });
}

/// Writes the cut's captions as `<title>.srt` in the job's folder - beside
/// the exported video on a desk - and on a phone copies it to where its
/// file manager shows it; says where. Nothing when the cut has no words.
fn keep_captions(folder: &Path, title: &str, placements: &[plan::Placement]) {
    let text = concat_generate::srt::from_placements(placements);
    if text.is_empty() {
        return;
    }
    let path = folder.join(format!("{}.srt", file_stem(title)));
    if let Err(error) = std::fs::write(&path, text) {
        log::warn!("generate: could not write {}: {error}", path.display());
        return;
    }
    let shown = path.to_string_lossy().into_owned();
    let handed = crate::platform::publish_file(path, |result| {
        on_ui(move |studio, _, _| match result {
            Ok(path) => studio.notify(&tf("generate.captionsFile", &[&path]), false),
            Err(error) => {
                log::warn!("generate: the captions file: {error}");
                studio.notify(&tf("generate.captionsFile", &[&shown]), false);
            }
        });
    });
    if !handed {
        let shown = folder
            .join(format!("{}.srt", file_stem(title)))
            .to_string_lossy()
            .into_owned();
        on_ui(move |studio, _, _| studio.notify(&tf("generate.captionsFile", &[&shown]), false));
    }
}

/// The editorial analysis as the script editor shows it: each part that
/// was worked out under its heading, lists a line an item; what does not
/// apply to the subject, left empty, is left out.
fn analysis_text(analysis: &concat_generate::editorial::Analysis) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (key, value) in [
        (t("script.analysisApproach"), &analysis.approach),
        (t("script.analysisQuestion"), &analysis.central_question),
        (t("script.analysisThesis"), &analysis.thesis),
        (t("script.analysisWhy"), &analysis.why_it_matters),
    ] {
        if !value.trim().is_empty() {
            parts.push(format!("{key}: {}", value.trim()));
        }
    }
    for (key, items) in [
        (t("script.analysisMechanisms"), &analysis.mechanisms),
        (t("script.analysisAssumptions"), &analysis.assumptions),
        (t("script.analysisMisunderstood"), &analysis.misunderstood),
        (t("script.analysisEvidence"), &analysis.evidence),
        (t("script.analysisObjections"), &analysis.counterarguments),
        (t("script.analysisLimits"), &analysis.limitations),
    ] {
        let listed: Vec<String> = items
            .iter()
            .map(|item| item.trim())
            .filter(|item| !item.is_empty())
            .map(|item| format!("• {item}"))
            .collect();
        if !listed.is_empty() {
            parts.push(format!("{key}:\n{}", listed.join("\n")));
        }
    }
    for (key, value) in [
        (t("script.analysisConclusion"), &analysis.conclusion),
        (t("script.analysisTakeaway"), &analysis.takeaway),
        (t("script.analysisOpening"), &analysis.opening),
    ] {
        if !value.trim().is_empty() {
            parts.push(format!("{key}: {}", value.trim()));
        }
    }
    let beats: Vec<String> = analysis
        .beats
        .iter()
        .map(|beat| beat.trim())
        .filter(|beat| !beat.is_empty())
        .enumerate()
        .map(|(n, beat)| format!("{}. {beat}", n + 1))
        .collect();
    if !beats.is_empty() {
        parts.push(format!(
            "{}:\n{}",
            t("script.analysisBeats"),
            beats.join("\n")
        ));
    }
    if !analysis.visuals.trim().is_empty() {
        parts.push(format!(
            "{}: {}",
            t("script.analysisVisuals"),
            analysis.visuals.trim()
        ));
    }
    parts.join("\n\n")
}

/// The editor's review as the script editor shows it: whether it passed
/// the script, whether the ending follows, the note on the whole, and how
/// many problems it found and scenes it had written again - each scene's
/// own problems are shown at the scene.
fn review_text(review: &concat_generate::editorial::Review) -> String {
    let mut parts = vec![if review.passed {
        t("script.reviewPassed")
    } else {
        t("script.reviewFailed")
    }];
    if !review.conclusion_follows {
        parts.push(t("script.reviewConclusion"));
    }
    if !review.summary.trim().is_empty() {
        parts.push(review.summary.trim().to_owned());
    }
    parts.push(tf(
        "generate.reviewed",
        &[&review.issues.len(), &review.revised.len()],
    ));
    // What it held against the whole, not one scene.
    for issue in review
        .issues
        .iter()
        .filter(|issue| issue.scene.trim().is_empty())
    {
        parts.push(format!("• {}", issue.problem.trim()));
    }
    parts.join("\n")
}

/// One picture's check as the script editor shows it at its shot: the
/// score, what is missing and what is wrong, and how to draw it; why it
/// was not looked at, when it was not.
fn check_text(check: &concat_generate::visual::Check) -> String {
    if !check.checked {
        return tf("script.checkNotChecked", &[&check.note]);
    }
    let mut lines = vec![if check.is_acceptable {
        tf("script.checkPassed", &[&check.semantic_alignment_score])
    } else {
        tf("script.checkFailed", &[&check.semantic_alignment_score])
    }];
    if !check.missing_elements.is_empty() {
        lines.push(tf(
            "script.checkMissing",
            &[&check.missing_elements.join(", ")],
        ));
    }
    if !check.visual_mismatches.is_empty() {
        lines.push(tf(
            "script.checkWrong",
            &[&check.visual_mismatches.join("; ")],
        ));
    }
    if !check.is_acceptable && !check.improvement_instructions.is_empty() {
        lines.push(tf("script.checkHow", &[&check.improvement_instructions]));
    }
    if !check.note.is_empty() {
        lines.push(check.note.clone());
    }
    lines.join("\n")
}

/// Every recording in the bin, with its length: what a music bed can be.
fn music_items(studio: &Studio) -> Vec<(String, f64)> {
    studio
        .project()
        .media
        .iter()
        .filter(|item| item.kind == MediaKind::Audio)
        .map(|item| (item.path.clone(), item.duration.unwrap_or(0.0)))
        .collect()
}

/// The sheet's choices that go with a format, as rows of their lists.
struct FormatLook {
    minutes: usize,
    aspect: usize,
    content_style: usize,
    level: usize,
    pictures: usize,
    caption_look: usize,
    clips: usize,
}

/// What a format sets in the sheet; None for no format, which sets
/// nothing.
fn format_look(format: VideoFormat) -> Option<FormatLook> {
    match format {
        VideoFormat::Custom => None,
        // A minute, upright, taught, in flat illustration with animated
        // captions, all stills: nothing in it holds long enough for a clip.
        VideoFormat::Minute => Some(FormatLook {
            minutes: 0,
            aspect: 1,
            content_style: 1,
            level: 1,
            pictures: 2,
            caption_look: 1,
            clips: 0,
        }),
        // Five minutes, wide, a documentary in photographs, a quarter of
        // its shots moving.
        VideoFormat::Story => Some(FormatLook {
            minutes: 2,
            aspect: 0,
            content_style: 0,
            level: 1,
            pictures: 1,
            caption_look: 0,
            clips: 1,
        }),
        // Three minutes, wide, taught to a beginner, illustrated, all
        // stills beside its drawings.
        VideoFormat::Lesson => Some(FormatLook {
            minutes: 1,
            aspect: 0,
            content_style: 1,
            level: 0,
            pictures: 2,
            caption_look: 0,
            clips: 0,
        }),
        // What are we building?: three minutes, upright, taught, every
        // shot a clip of the same object growing, captions a few words at
        // a time.
        VideoFormat::Build => Some(FormatLook {
            minutes: 1,
            aspect: 1,
            content_style: 1,
            level: 1,
            pictures: 0,
            caption_look: 1,
            clips: CLIP_SHARES.len() - 1,
        }),
    }
}

/// The pictures a logo is picked from: the one already picked, wherever
/// it is, then every picture in the bin.
fn logo_items(studio: &Studio) -> Vec<String> {
    let kept = studio.prefs.generate.brand_logo.clone();
    let mut items: Vec<String> = Vec::new();
    if !kept.trim().is_empty() && Path::new(&kept).is_file() {
        items.push(kept);
    }
    for item in &studio.project().media {
        if item.kind == MediaKind::Image && !items.contains(&item.path) {
            items.push(item.path.clone());
        }
    }
    items
}

/// What the channel's opening and closing say, when it wants them on its
/// videos and has a name to show.
fn brand_of(prefs: &crate::prefs::GeneratePrefs) -> Option<concat_generate::job::Brand> {
    (prefs.brand_bumpers && !prefs.channel_name.trim().is_empty()).then(|| {
        concat_generate::job::Brand {
            name: prefs.channel_name.trim().to_owned(),
            opening: prefs.channel_opening.trim().to_owned(),
            closing: prefs.channel_closing.trim().to_owned(),
            colour: prefs.brand_color.trim().to_owned(),
        }
    })
}

/// The person's own OAuth client, as typed.
fn youtube_client(prefs: &crate::prefs::GeneratePrefs) -> youtube::Client {
    youtube::Client {
        id: prefs.youtube_client_id.trim().to_owned(),
        secret: prefs.youtube_client_secret.trim().to_owned(),
    }
}

/// The videos the channel put up from here, id and title, oldest first,
/// kept in its folder.
fn read_uploads(channel_dir: &Path) -> Vec<(String, String)> {
    std::fs::read(channel_dir.join("uploads.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Adds a video put up to the channel's list.
fn keep_upload(channel_dir: &Path, id: &str, title: &str) -> Result<(), String> {
    let mut uploads = read_uploads(channel_dir);
    uploads.retain(|(kept, _)| kept != id);
    uploads.push((id.to_owned(), title.to_owned()));
    std::fs::create_dir_all(channel_dir).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(&uploads).map_err(|error| error.to_string())?;
    std::fs::write(channel_dir.join("uploads.json"), bytes).map_err(|error| error.to_string())
}

/// The row of `calendar::PER_WEEK` the plan holds: three a week unless
/// chosen.
fn per_week_row(prefs: &crate::prefs::GeneratePrefs) -> usize {
    prefs
        .plan_per_week
        .unwrap_or(2)
        .min(concat_generate::calendar::PER_WEEK.len() - 1)
}

/// The channel in use as the writer is told it when designing or planning
/// for it: the fields filled in, a line each.
fn channel_text(prefs: &crate::prefs::GeneratePrefs) -> String {
    [
        ("Channel", &prefs.channel_name),
        ("Tagline", &prefs.channel_tagline),
        ("Who watches", &prefs.channel_audience),
        ("How it sounds", &prefs.channel_voice),
        ("What it covers", &prefs.channel_topics),
        ("Rules", &prefs.channel_rules),
    ]
    .iter()
    .filter(|(_, value)| !value.trim().is_empty())
    .map(|(label, value)| format!("{label}: {}", value.trim()))
    .collect::<Vec<_>>()
    .join("\n")
}

/// A channel's look as designed, and its logo and banner as drawn - or
/// why they were not.
pub type Look = (identity::Identity, Result<(PathBuf, PathBuf), String>);

/// What goes up with the video, written and kept, and the thumbnail's
/// picture probed - or why there is none.
pub type Published = (Publishing, Result<NewMedia, String>);

/// A shot's small picture as a worker hands it over: its stem, width,
/// height and RGBA pixels, and its likeness when it has one.
pub type Thumb = (String, u32, u32, Vec<u8>, Option<Likeness>);

/// Thumbnails handed to the sheet at once: the first show at once on a
/// phone, and a long video's hundred do not wait for one another.
const THUMBS_AT_ONCE: usize = 6;

/// A shot's picture, small, and its likeness. Read by what the file holds
/// rather than by its name: a service asked for a PNG may answer with a
/// JPEG or a WebP, which is saved under the name asked for.
fn thumbnail(path: &Path) -> Option<(image::RgbaImage, Option<Likeness>)> {
    let picture = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let small = picture.thumbnail(240, 240);
    let likeness = Likeness::of(&small);
    Some((small.to_rgba8(), likeness))
}

/// What a picture looks like, roughly: where it is lighter and darker
/// (a difference hash, 64 bits) and its colour on the whole. Two shots of
/// one scene that came out the same picture in other words - the same
/// subject, framed alike - agree on both.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Likeness {
    shape: u64,
    colour: [u8; 3],
}

impl Likeness {
    /// Of a picture with detail enough to tell; `None` for a colour card
    /// or another wash, which every draft shot is and which would read
    /// as alike.
    fn of(picture: &image::DynamicImage) -> Option<Likeness> {
        use image::imageops::FilterType;
        let grey = picture
            .resize_exact(32, 32, FilterType::Triangle)
            .to_luma8();
        let mut detail = 0.0;
        for y in 1..31 {
            for x in 1..31 {
                let at = |dx: i32, dy: i32| {
                    f64::from(grey.get_pixel((x + dx) as u32, (y + dy) as u32)[0])
                };
                detail += (4.0 * at(0, 0) - at(-1, 0) - at(1, 0) - at(0, -1) - at(0, 1)).abs();
            }
        }
        if detail / (30.0 * 30.0) < LEAST_DETAIL {
            return None;
        }
        let small = picture.resize_exact(9, 8, FilterType::Triangle).to_luma8();
        let mut shape = 0u64;
        for y in 0..8 {
            for x in 0..8 {
                shape <<= 1;
                if small.get_pixel(x, y)[0] > small.get_pixel(x + 1, y)[0] {
                    shape |= 1;
                }
            }
        }
        let one = picture.resize_exact(1, 1, FilterType::Triangle).to_rgb8();
        Some(Likeness {
            shape,
            colour: one.get_pixel(0, 0).0,
        })
    }

    /// Near enough to read as the same picture twice.
    fn alike(&self, other: &Likeness) -> bool {
        let colour: u32 = self
            .colour
            .iter()
            .zip(other.colour)
            .map(|(a, b)| u32::from(a.abs_diff(b)))
            .sum();
        (self.shape ^ other.shape).count_ones() <= MOST_SHAPE_BITS && colour <= MOST_COLOUR
    }
}

/// The least mean edge, on a 32x32 grey copy, of a picture worth
/// comparing: a photograph has many times this, a colour card none.
const LEAST_DETAIL: f64 = 3.0;
/// Of the 64 bits of shape, the most two alike pictures differ in.
const MOST_SHAPE_BITS: u32 = 10;
/// The most two alike pictures' colours differ by, summed over red, green
/// and blue.
const MOST_COLOUR: u32 = 60;

/// The window's session as the cut lays itself down through it: every
/// command folded into one undo step.
struct Session<'a>(&'a mut Studio);

impl Editing for Session<'_> {
    fn apply(&mut self, command: Command) -> Option<String> {
        self.0.apply_gesture("generate", command)
    }

    fn lanes(&self) -> Vec<String> {
        self.0
            .timeline()
            .tracks
            .iter()
            .map(|track| track.id.clone())
            .collect()
    }

    fn media_of(&self, path: &str) -> Option<String> {
        self.0
            .project()
            .media
            .iter()
            .find(|item| item.path == path)
            .map(|item| item.id.clone())
    }
}

/// What the review found, in the interface's language.
fn finding_text(finding: &review::Finding) -> String {
    use review::Finding;
    match finding {
        Finding::StoodIn(n) => tf("generate.reviewStoodIn", &[n]),
        Finding::Unread(n) => tf("generate.reviewUnread", &[n]),
        Finding::LeftOut(n) => tf("generate.reviewLeftOut", &[n]),
        Finding::NotDrawn(n) => tf("generate.reviewNotDrawn", &[n]),
        Finding::Repeats(n) => tf("generate.reviewRepeats", &[n]),
        Finding::Silence { seconds, at } => tf(
            "generate.reviewSilence",
            &[&format!("{seconds:.0}"), &clock(*at)],
        ),
    }
}

/// What to say when the writer picked has no key: the Anthropic key for
/// Claude, the OpenAI key for ChatGPT.
fn need_writer_key(prefs: &crate::prefs::GeneratePrefs) -> String {
    match prefs.writer {
        1 => t("generate.needOpenAiWriterKey"),
        _ => t("generate.needAnthropicKey"),
    }
}

/// What the check before publishing found, in the interface's language.
fn warning_text(warning: &checklist::Warning) -> String {
    use checklist::Warning;
    match warning {
        Warning::Unsourced(n) => tf("generate.checkUnsourced", &[n]),
        Warning::Disputed(n) => tf("generate.checkDisputed", &[n]),
        Warning::MadeBefore(title) => tf("generate.checkMadeBefore", &[title]),
        Warning::OwnMusic(name) => tf("generate.checkOwnMusic", &[name]),
        Warning::MadeMusic => t("generate.checkMadeMusic"),
        Warning::TooLong { name, seconds } => {
            tf("generate.checkTooLong", &[name, &clock(*seconds)])
        }
        Warning::AiLabel => t("generate.checkAiLabel"),
    }
}

/// A key told apart from another without being kept or written anywhere:
/// what a message carries in its place, every message being logged.
fn fingerprint(key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// One file of Export everything.
enum Exporting {
    /// A timeline of the video's project: its id and the file's name.
    Timeline(String, String),
    /// A Short, from its own project: its folder and the file's name.
    Short(String, String),
}

/// Opens the project in `path` in the window, the open one saved and
/// closed first, as the start screen does; nothing when it is open
/// already. A project that could not be saved stays open, and says why.
fn switch_project(studio: &mut Studio, path: &str) -> Result<(), String> {
    if studio
        .session
        .as_ref()
        .is_some_and(|session| session.path() == path)
    {
        return Ok(());
    }
    let info = concat_host::projects::open(path)?;
    if studio.session.is_some() {
        studio.close_project();
        if studio.session.is_some() {
            return Err(t("generate.couldNotLeave"));
        }
    }
    studio.open_project(info)
}

/// A cut ready to lay down; see [`GeneratePane::prepare_cut`].
struct Prepared {
    placements: Vec<plan::Placement>,
    media: HashMap<String, NewMedia>,
    options: Options,
    /// The cards a captions file is written from, when there are any.
    captioned: Option<Vec<plan::Placement>>,
}

/// A project of its own, made beside the window's, as the cut lays itself
/// down in it: the window's project is not touched, and the cut is no undo
/// step of it.
struct Apart(concat_project::Editor);

impl Editing for Apart {
    fn apply(&mut self, command: Command) -> Option<String> {
        self.0
            .apply(command)
            .inspect_err(|error| log::warn!("generate: a Short's project: {error:?}"))
            .ok()
            .and_then(|outcome| outcome.created_id)
    }

    fn lanes(&self) -> Vec<String> {
        self.0
            .project()
            .active()
            .tracks
            .iter()
            .map(|track| track.id.clone())
            .collect()
    }

    fn media_of(&self, path: &str) -> Option<String> {
        self.0
            .project()
            .media
            .iter()
            .find(|item| item.path == path)
            .map(|item| item.id.clone())
    }
}

/// Makes the project `name` beside `beside` (a project folder) holding
/// the cut `lay` lays down, saves it and puts it on the recents list kept
/// under `config`. A name already taken there gets a number after it: a
/// project is never written over. The new project's folder.
fn project_apart(
    beside: &Path,
    config: &Path,
    name: &str,
    (width, height): (u32, u32),
    lay: impl FnOnce(&mut Apart) -> Result<(), String>,
) -> Result<String, String> {
    use concat_host::projects;
    let location = beside
        .parent()
        .ok_or("the video's project has no folder around it")?;
    let mut info = None;
    let mut error = String::new();
    for n in 1..100 {
        let tried = if n == 1 {
            name.trim().to_owned()
        } else {
            format!("{} {n}", name.trim())
        };
        match projects::create(&location.to_string_lossy(), &tried, width, height, 30, 1) {
            Ok(made) => {
                info = Some(made);
                break;
            }
            Err(why) if why.contains("already exists") => error = why,
            Err(why) => return Err(why),
        }
    }
    let info = info.ok_or(error)?;
    let settings = concat_project::DocumentSettings {
        name: info.name.clone(),
        width,
        height,
        rate_num: 30,
        rate_den: 1,
    };
    let mut apart = Apart(concat_project::Editor::with_video(settings.video()));
    let first = apart.0.project().active_timeline_id.clone();
    lay(&mut apart)?;
    // The cut is on a timeline of its own: the empty one a new project
    // starts with goes.
    if apart.0.project().timelines.len() > 1 {
        apart.apply(Command::RemoveTimeline { timeline_id: first });
    }
    projects::save(&info.path, &apart.0.to_document(&settings))?;
    if let Err(why) = projects::remember(config, &info) {
        log::warn!("generate: the recents list: {why}");
    }
    Ok(info.path)
}

/// The picture models the chosen maker's key can run, as last asked.
#[derive(Default)]
struct PictureModels {
    /// Asked of: 1 OpenAI, 2 Gemini; 0 for none asked.
    maker: usize,
    /// With the key of this fingerprint; see [`fingerprint`].
    key: u64,
    /// The answer is on its way.
    asking: bool,
    /// The models, the default first; empty until they come, or when the
    /// maker would not list them.
    names: Vec<String>,
    /// Why there is no list.
    error: String,
}

/// Seconds as minutes and seconds: "2:05".
fn clock(seconds: f64) -> String {
    let whole = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// Where the shots come from, as the sheet's choices say.
fn shots_of(prefs: &crate::prefs::GeneratePrefs) -> Shots {
    match prefs.shots {
        1 => Shots::Models {
            pictures: match prefs.picture_maker {
                1 => makers::Pictures::OpenAi {
                    key: prefs.openai_key.clone(),
                    model: prefs.openai_image_model.clone(),
                },
                2 => makers::Pictures::Gemini {
                    key: prefs.gemini_key.clone(),
                    model: prefs.gemini_image_model.clone(),
                },
                _ => makers::Pictures::Replicate {
                    token: prefs.replicate_token.clone(),
                    model: prefs.replicate_image_model.clone(),
                },
            },
            clips: Some(match prefs.clip_maker {
                1 => makers::Clips::Kling {
                    access_key: prefs.kling_access_key.clone(),
                    secret_key: prefs.kling_secret_key.clone(),
                    model: prefs.kling_model.clone(),
                },
                2 => makers::Clips::Seedance {
                    key: prefs.seedance_key.clone(),
                    model: prefs.seedance_model.clone(),
                },
                _ => makers::Clips::Replicate {
                    token: prefs.replicate_token.clone(),
                    model: video_model(&prefs.replicate_model).to_owned(),
                    input: REPLICATE_INPUT.to_owned(),
                },
            }),
            replicate: prefs.replicate_token.clone(),
        },
        2 => Shots::Folder,
        _ => Shots::Draft,
    }
}

/// The job and the shot a file is, when it is one of a job's shots.
fn shot_of(path: &Path) -> Option<(Job, String)> {
    let job = Job::of_shot(path)?;
    let stem = job.stem_of(path)?;
    Some((job, stem))
}

/// A still rather than a clip, by its file's extension.
fn is_still(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "webp"
        )
    })
}

/// The video model to use: the one named, or the default.
fn video_model(named: &str) -> &str {
    if named.trim().is_empty() {
        DEFAULT_VIDEO_MODEL
    } else {
        named.trim()
    }
}

/// A price or a limit typed in the sheet, in dollars: None when it is
/// empty or not a number of them.
fn amount(text: &str) -> Option<f64> {
    text.trim()
        .trim_start_matches('$')
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Who holds the phone's keep-alive while a run makes a video, wherever
/// the window has gone since.
const MAKING: &str = "making";

/// How far a run is, roughly, from what it says: for the pill and the
/// notification, which see it from wherever the window has gone.
fn rough(step: &Step) -> Option<(f32, String)> {
    match step {
        Step::Voice { n, of, .. } => Some((
            0.05 + 0.2 * (*n as f32 / (*of).max(1) as f32),
            tf("generate.readingScene", &[n, of]),
        )),
        Step::Shot { n, of, .. } => Some((
            0.25 + 0.7 * ((*n).saturating_sub(1) as f32 / (*of).max(1) as f32),
            tf("generate.makingShot", &[n, of]),
        )),
        Step::Made { n, of, .. } => Some((
            0.25 + 0.7 * (*n as f32 / (*of).max(1) as f32),
            tf("generate.makingShot", &[n, of]),
        )),
        Step::Music => Some((0.95, t("generate.makingMusic"))),
        Step::Probing => Some((0.97, t("generate.readingFiles"))),
        Step::Checking { n, of } => Some((
            0.97 + 0.03 * (*n as f32 / (*of).max(1) as f32),
            tf("generate.checkingPicture", &[n, of]),
        )),
        Step::Redrawing { n, of } => Some((
            0.97 + 0.03 * (*n as f32 / (*of).max(1) as f32),
            tf("generate.redrawingPicture", &[n, of]),
        )),
    }
}

/// The music row that is music made for the video by MusicGen.
const AI_MUSIC: usize = 1;

/// What each thing costs: the prices typed in the sheet, the defaults'
/// where none is.
fn prices(studio: &Studio) -> Prices {
    let prefs = &studio.prefs.generate;
    let defaults = Prices::default();
    // A service reached directly has its own price, whatever was typed
    // for Replicate's models; about what each charges for one picture at
    // the size asked, and one five-second clip.
    let still = match prefs.picture_maker {
        1 => OPENAI_IMAGE_PRICE,
        2 => GEMINI_IMAGE_PRICE,
        _ => amount(&prefs.image_price).unwrap_or(defaults.still),
    };
    let clip = match prefs.clip_maker {
        1 => KLING_CLIP_PRICE,
        2 => SEEDANCE_CLIP_PRICE,
        _ => amount(&prefs.clip_price).unwrap_or(defaults.clip),
    };
    Prices {
        still,
        clip,
        ..defaults
    }
}

/// The most a run may spend; None for no limit.
fn spend_limit(studio: &Studio) -> Option<f64> {
    amount(&studio.prefs.generate.spend_limit)
}

/// Dollars as the sheet shows them: cents, or a tenth of one below a cent.
fn dollars(value: f64) -> String {
    if value > 0.0 && value < 0.01 {
        format!("{value:.3}")
    } else {
        format!("{value:.2}")
    }
}

/// About what one picture costs from OpenAI at high quality, and from
/// Gemini; one five-second clip from Kling at standard, and from Seedance
/// Pro at 1080p. Estimates for the sheet's sum and the spend limit, not
/// what the services bill.
const OPENAI_IMAGE_PRICE: f64 = 0.25;
const GEMINI_IMAGE_PRICE: f64 = 0.04;
const KLING_CLIP_PRICE: f64 = 0.30;
const SEEDANCE_CLIP_PRICE: f64 = 0.60;

/// The professional picture model, beside the default economical one:
/// finer detail and truer light, at about four cents a picture.
const PROFESSIONAL_IMAGE_MODEL: &str = "black-forest-labs/flux-1.1-pro";
const PROFESSIONAL_IMAGE_PRICE: &str = "0.04";

/// The best picture model: four times the pixels of the others - about
/// 2750x1550 at 16:9 against their 1344x768 - so a picture is brought down
/// to the frame rather than enlarged and softened up to it, at about six
/// cents a picture. The owner found the pictures soft beside an image
/// ChatGPT made.
const BEST_IMAGE_MODEL: &str = "black-forest-labs/flux-1.1-pro-ultra";
const BEST_IMAGE_PRICE: &str = "0.06";

/// The audience level's row: general until one is chosen.
fn level_row(prefs: &crate::prefs::GeneratePrefs) -> usize {
    prefs.level.unwrap_or(1).min(Level::ALL.len() - 1)
}

/// The channel as the writer is told it: one line for each thing the
/// person filled in, nothing for what they left empty.
fn channel_profile(prefs: &crate::prefs::GeneratePrefs) -> String {
    [
        ("Name", &prefs.channel_name),
        ("Audience", &prefs.channel_audience),
        ("Voice and tone", &prefs.channel_voice),
        ("Opens every video with", &prefs.channel_opening),
        ("Closes every video with", &prefs.channel_closing),
        (
            "A script it likes - write in its manner, not about its subject",
            &prefs.channel_example,
        ),
        ("The subjects it covers", &prefs.channel_topics),
        (
            "Its editorial rules - every script keeps to them",
            &prefs.channel_rules,
        ),
        (
            "Videos it has already made - never make one of them again, never \
             retell its story; refer back to one only where that truly helps",
            &made_before(prefs),
        ),
    ]
    .iter()
    .filter(|(_, value)| !value.trim().is_empty())
    .map(|(label, value)| format!("{label}: {}", value.trim()))
    .collect::<Vec<_>>()
    .join("\n")
}

/// The videos the channel has made, as the writer is told them: every
/// one it remembers, newest first, one after another, so the count the
/// sheet shows is the count the writer is told not to repeat.
fn made_before(prefs: &crate::prefs::GeneratePrefs) -> String {
    prefs
        .made_before()
        .iter()
        .rev()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("; ")
}

/// `board` with its scenes as long as the cut lays them out from their
/// voices, where the voice was made: a Short's minute is measured so.
fn voice_timed(job: &Job, board: &Storyboard) -> Storyboard {
    let made = job
        .gather(board, true)
        .map(|made| made.files)
        .unwrap_or_default();
    shorts::timed(board, &made)
}

/// The file name a video's title is written under: what a file system
/// refuses made a dash, and "video" for none.
fn file_stem(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '-'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_owned();
    if name.is_empty() {
        "video".to_owned()
    } else {
        name
    }
}

/// `name`, or `name (2)`, `name (3)`... the first that `taken` does not
/// hold, its file name compared without case: each video of Export
/// everything writes a file of its own.
fn unique_name(name: &str, taken: &mut HashSet<String>) -> String {
    let base = name.trim();
    let mut candidate = base.to_owned();
    let mut n = 2;
    while !taken.insert(file_stem(&candidate).to_lowercase()) {
        candidate = format!("{base} ({n})");
        n += 1;
    }
    candidate
}

/// The video's tags as hashtags: the first eight, each one word, joined.
fn hashtags(tags: &[String]) -> String {
    tags.iter()
        .map(|tag| tag.split_whitespace().collect::<String>())
        .filter(|tag| !tag.is_empty())
        .take(8)
        .map(|tag| format!("#{}", tag.trim_start_matches('#')))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a series is saved under the ready channel's name.
fn has_la_logica(prefs: &crate::prefs::GeneratePrefs) -> bool {
    prefs
        .templates
        .iter()
        .any(|template| template.name.trim().to_lowercase() == crate::prefs::LA_LOGICA)
}

/// The channels' list row of the channel in use: the series saved under
/// its name, or the ready channel's row; 0 when it is none of them.
fn channel_row(prefs: &crate::prefs::GeneratePrefs) -> i32 {
    let name = prefs.channel_name.trim().to_lowercase();
    if name.is_empty() {
        return 0;
    }
    match prefs
        .templates
        .iter()
        .position(|template| template.name.trim().to_lowercase() == name)
    {
        Some(row) => row as i32 + 1,
        None if name == crate::prefs::LA_LOGICA && !has_la_logica(prefs) => {
            prefs.templates.len() as i32 + 1
        }
        None => 0,
    }
}

/// A card's lines as the script editor shows them: one a line, what it
/// is and its figure after a dash.
fn card_lines(graphic: &concat_generate::storyboard::Graphic) -> String {
    graphic
        .items
        .iter()
        .map(|item| match item.value.trim() {
            "" => item.label.trim().to_owned(),
            value => format!("{} — {value}", item.label.trim()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The card's lines read back from the script editor.
fn read_card_lines(text: &str) -> Vec<concat_generate::storyboard::GraphicItem> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (label, value) = line
                .rsplit_once('—')
                .or_else(|| line.rsplit_once(" - "))
                .unwrap_or((line, ""));
            concat_generate::storyboard::GraphicItem {
                label: label.trim().to_owned(),
                value: value.trim().to_owned(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Short is a project of its own beside the video's: its cut on its
    /// one timeline, on the recents list, and a name taken gets a number,
    /// never written over.
    #[test]
    fn a_short_is_a_project_beside_the_video() {
        let root = std::env::temp_dir().join(format!("concat-apart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let video = root.join("Video");
        let config = root.join("config");
        std::fs::create_dir_all(&video).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        let lay = |apart: &mut Apart| {
            let id = apart.apply(Command::AddTimeline).ok_or("no timeline")?;
            apart.apply(Command::RenameTimeline {
                timeline_id: id,
                name: "Short".into(),
            });
            Ok(())
        };
        let first = project_apart(&video, &config, "Short", (1080, 1920), lay).unwrap();
        let second = project_apart(&video, &config, "Short", (1080, 1920), lay).unwrap();
        assert_ne!(first, second);
        assert!(second.ends_with("Short 2"), "{second}");
        let document = concat_host::projects::read_document(&first)
            .unwrap()
            .unwrap();
        let editor = concat_project::Editor::from_document(&document).unwrap();
        let names: Vec<&str> = editor
            .project()
            .timelines
            .iter()
            .map(|timeline| timeline.name.as_str())
            .collect();
        assert_eq!(names, ["Short"]);
        let recents = concat_host::projects::list(&config);
        assert!(recents.iter().any(|info| info.path == first));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each video of Export everything gets a file name of its own, two
    /// names that come to the same file told apart.
    #[test]
    fn export_names_are_made_unique() {
        let mut taken = HashSet::new();
        assert_eq!(unique_name("La mente", &mut taken), "La mente");
        assert_eq!(unique_name("la mente ", &mut taken), "la mente (2)");
        assert_eq!(unique_name("a/b", &mut taken), "a/b");
        assert_eq!(unique_name("a-b", &mut taken), "a-b (2)");
        assert_eq!(file_stem(" / "), "-");
        assert_eq!(file_stem("  "), "video");
    }

    /// Shorts titled alike - the ones made up where the writer picked too
    /// few - each get a name, so their tabs and captions files differ.
    #[test]
    fn shorts_titled_alike_are_told_apart() {
        let mut taken = HashSet::new();
        let names: Vec<String> = ["Rome - Short", "Rome - Short", "Rome - Short", "Gaul"]
            .iter()
            .map(|title| unique_name(title, &mut taken))
            .collect();
        assert_eq!(
            names,
            [
                "Rome - Short",
                "Rome - Short (2)",
                "Rome - Short (3)",
                "Gaul"
            ]
        );
    }

    /// The video's tags as hashtags, one word each, eight at most.
    #[test]
    fn tags_become_hashtags() {
        let tags: Vec<String> = ["Spanish politics", "#logic", ""]
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(hashtags(&tags), "#Spanishpolitics #logic");
        let many: Vec<String> = (0..12).map(|n| n.to_string()).collect();
        assert_eq!(hashtags(&many).split(' ').count(), 8);
    }
    use image::{DynamicImage, Rgb, RgbImage};

    /// A sun over hills on a striped ground, `sun_x` across: detail
    /// enough to compare, and a shape that moves with the sun.
    fn scene(sun_x: f64, light: u8) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_fn(320, 180, |x, y| {
            let (fx, fy) = (f64::from(x), f64::from(y));
            let sun = ((fx - sun_x).powi(2) + (fy - 50.0).powi(2)).sqrt() < 30.0;
            let hill = fy > 120.0 + 20.0 * (fx / 40.0).sin();
            let stripe = if (x / 6 + y / 6) % 2 == 0 { 25 } else { 0 };
            match (sun, hill) {
                (true, _) => Rgb([250, 220, 120]),
                (_, true) => Rgb([40, 110 + stripe, 50]),
                _ => Rgb([90 + stripe, 140, 200].map(|c: u8| c.saturating_add(light))),
            }
        }))
    }

    #[test]
    fn a_colour_card_is_not_compared() {
        let card = DynamicImage::ImageRgb8(RgbImage::from_fn(320, 180, |x, y| {
            Rgb([(x / 2) as u8, (y / 2) as u8, 120])
        }));
        assert_eq!(Likeness::of(&card), None);
    }

    #[test]
    fn the_same_picture_a_little_lighter_reads_as_alike() {
        let a = Likeness::of(&scene(80.0, 0)).unwrap();
        let b = Likeness::of(&scene(84.0, 12)).unwrap();
        assert!(a.alike(&b));
    }

    #[test]
    fn another_framing_does_not() {
        let a = Likeness::of(&scene(80.0, 0)).unwrap();
        let b = Likeness::of(&scene(240.0, 0)).unwrap();
        assert!(!a.alike(&b));
        let flipped = Likeness::of(&scene(80.0, 0).fliph()).unwrap();
        assert!(!a.alike(&flipped));
    }
}
