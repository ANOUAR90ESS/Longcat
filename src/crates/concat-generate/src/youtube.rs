// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! YouTube, the last stage of the channel manager (ROADMAP, "Channel
//! manager"): signing in to the person's own channel, putting a video up,
//! and reading its numbers back.
//!
//! The app has no Google project of its own: the person makes an OAuth
//! client in theirs ("Desktop app") and gives its id and secret, as they
//! give every other service's key. Signing in is Google's installed-app
//! flow: the browser opens Google's page, the person allows it, and Google
//! sends the browser back to a port this app listens on, on the device
//! itself, with a code; the code and a PKCE verifier only this run knew
//! are traded for a refresh token, kept on the device like the other keys.
//! The password never reaches the app.
//!
//! A video goes up through YouTube's resumable upload, its title,
//! description and tags from what the writer wrote for it. A project not
//! audited by Google may only put videos up as private: the person makes
//! them public in YouTube Studio, or has the project audited.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::providers::{agent, refused};

/// Google's sign-in page.
const AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Where a code or refresh token is traded for an access token.
const TOKEN: &str = "https://oauth2.googleapis.com/token";
/// YouTube's Data API.
const API: &str = "https://www.googleapis.com/youtube/v3";
/// Where videos go up.
const UPLOAD: &str = "https://www.googleapis.com/upload/youtube/v3/videos";
/// What the app asks to do: put videos up, and read the channel's numbers.
const SCOPES: &str = "https://www.googleapis.com/auth/youtube.upload \
    https://www.googleapis.com/auth/youtube.readonly";
/// How long the browser has to come back with a code.
const SIGN_IN_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// The person's OAuth client, from their own Google Cloud project.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Client {
    /// Its id, `….apps.googleusercontent.com`.
    pub id: String,
    /// Its secret. An installed app's is not kept from the person: Google
    /// says so; PKCE is what protects the sign-in.
    pub secret: String,
}

impl Client {
    /// Whether both are given.
    pub fn is_set(&self) -> bool {
        !self.id.trim().is_empty() && !self.secret.trim().is_empty()
    }
}

/// A sign-in under way: the page to open, and what its answer is checked
/// against.
pub struct SignIn {
    /// Google's page, for the browser.
    pub url: String,
    listener: TcpListener,
    redirect: String,
    state: String,
    verifier: String,
}

/// Random bytes, hex, from the standard library's randomly seeded hasher:
/// enough for a state and a PKCE verifier, without a crate for it.
fn random_hex(bytes: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    let mut round: u64 = 0;
    while out.len() < bytes * 2 {
        // Each RandomState is seeded from the system's randomness, and
        // each one made after it differs again.
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(round);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos()),
        );
        hasher.write_u32(std::process::id());
        let digest = Sha256::digest(hasher.finish().to_le_bytes());
        out.extend(digest.iter().map(|byte| format!("{byte:02x}")));
        round += 1;
    }
    out.truncate(bytes * 2);
    out
}

/// PKCE's challenge for `verifier`: its SHA-256, base64url without padding.
pub fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `text` as a URL's query value.
fn encoded(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Google's sign-in page for `client`, sending the browser back to
/// `redirect` with `state`, its code bound to `verifier`.
pub fn auth_url(client: &Client, redirect: &str, state: &str, verifier: &str) -> String {
    format!(
        "{AUTH}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}\
         &code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        encoded(client.id.trim()),
        encoded(redirect),
        encoded(SCOPES),
        encoded(state),
        challenge(verifier)
    )
}

/// Starts signing in: a port on this device to come back to, and the page
/// to open.
pub fn begin(client: &Client) -> Result<SignIn, String> {
    if !client.is_set() {
        return Err("YouTube needs the OAuth client's id and secret".to_owned());
    }
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("a port to sign in on: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let redirect = format!("http://127.0.0.1:{port}");
    let state = random_hex(16);
    let verifier = random_hex(48);
    Ok(SignIn {
        url: auth_url(client, &redirect, &state, &verifier),
        listener,
        redirect,
        state,
        verifier,
    })
}

/// The code in the first line of the browser's request back -
/// `GET /?state=…&code=… HTTP/1.1` - when its state is `state`; why not
/// otherwise.
pub fn code_of(request_line: &str, state: &str) -> Result<String, String> {
    let target = request_line
        .split_whitespace()
        .nth(1)
        .ok_or("the browser came back with nothing")?;
    let query = target.split_once('?').map_or("", |(_, query)| query);
    let value = |key: &str| {
        query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            (name == key).then(|| decoded(value))
        })
    };
    if let Some(error) = value("error") {
        return Err(format!("Google said no: {error}"));
    }
    if value("state").as_deref() != Some(state) {
        return Err("the browser came back from another sign-in".to_owned());
    }
    value("code").ok_or_else(|| "the browser came back without a code".to_owned())
}

/// A query value decoded: `%2F` and `+`.
fn decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' if at + 2 < bytes.len() => match u8::from_str_radix(&text[at + 1..at + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    at += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl SignIn {
    /// Waits for the browser to come back, answers it with a page that
    /// says to return to the app, and trades the code for a refresh token.
    pub fn finish(self, client: &Client) -> Result<String, String> {
        self.listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let started = Instant::now();
        let (mut stream, _) = loop {
            match self.listener.accept() {
                Ok(accepted) => break accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() > SIGN_IN_DEADLINE {
                        return Err("the sign-in was not finished in five minutes".to_owned());
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(error) => return Err(error.to_string()),
            }
        };
        stream
            .set_nonblocking(false)
            .and_then(|()| stream.set_read_timeout(Some(Duration::from_secs(10))))
            .map_err(|error| error.to_string())?;
        let mut request = vec![0u8; 8192];
        let read = stream
            .read(&mut request)
            .map_err(|error| error.to_string())?;
        let request = String::from_utf8_lossy(&request[..read]).into_owned();
        let line = request.lines().next().unwrap_or_default();
        let code = code_of(line, &self.state);
        let page = match &code {
            Ok(_) => "LongCut is connected to YouTube. You can go back to the app.",
            Err(_) => "LongCut could not connect to YouTube. Go back to the app to see why.",
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let code = code?;
        let answer: Value = agent()
            .post(TOKEN)
            .send_form(&[
                ("code", code.as_str()),
                ("client_id", client.id.trim()),
                ("client_secret", client.secret.trim()),
                ("redirect_uri", self.redirect.as_str()),
                ("grant_type", "authorization_code"),
                ("code_verifier", self.verifier.as_str()),
            ])
            .map_err(|error| refused("Google", error))?
            .into_json()
            .map_err(|error| error.to_string())?;
        answer["refresh_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| "Google gave no refresh token: sign in again".to_owned())
    }
}

/// An access token for an hour, traded for the refresh token kept.
pub fn access(client: &Client, refresh_token: &str) -> Result<String, String> {
    if refresh_token.trim().is_empty() {
        return Err("connect YouTube first".to_owned());
    }
    let answer: Value = agent()
        .post(TOKEN)
        .send_form(&[
            ("client_id", client.id.trim()),
            ("client_secret", client.secret.trim()),
            ("refresh_token", refresh_token.trim()),
            ("grant_type", "refresh_token"),
        ])
        .map_err(|error| refused("Google", error))?
        .into_json()
        .map_err(|error| error.to_string())?;
    answer["access_token"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "Google gave no access token: connect YouTube again".to_owned())
}

/// The channel signed in to: its title.
pub fn channel_title(access: &str) -> Result<String, String> {
    let answer: Value = agent()
        .get(&format!("{API}/channels?part=snippet&mine=true"))
        .set("authorization", &format!("Bearer {access}"))
        .call()
        .map_err(|error| refused("YouTube", error))?
        .into_json()
        .map_err(|error| error.to_string())?;
    answer["items"][0]["snippet"]["title"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "this Google account has no YouTube channel".to_owned())
}

/// Who may see a video once it is up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    /// Only the channel's owner.
    #[default]
    Private,
    /// Whoever has the link.
    Unlisted,
    /// Everyone.
    Public,
}

impl Privacy {
    /// The row of a list of the three, in this order.
    pub fn of_row(row: usize) -> Privacy {
        match row {
            1 => Privacy::Unlisted,
            2 => Privacy::Public,
            _ => Privacy::Private,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Privacy::Private => "private",
            Privacy::Unlisted => "unlisted",
            Privacy::Public => "public",
        }
    }
}

/// What goes up with a video.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Upload {
    /// Its title.
    pub title: String,
    /// Its description.
    pub description: String,
    /// Its search tags.
    pub tags: Vec<String>,
    /// Who may see it.
    pub privacy: Privacy,
    /// `#Shorts` added to its title and description: a Short.
    pub short: bool,
}

/// The metadata YouTube is sent: title held to its hundred characters,
/// description to its five thousand bytes, tags to five hundred characters
/// in all, none with angle brackets, which YouTube refuses.
pub fn metadata(upload: &Upload) -> Value {
    let clean = |text: &str| text.replace(['<', '>'], "");
    let mut title = clean(upload.title.trim());
    if upload.short && !title.contains("#Shorts") {
        title = format!("{title} #Shorts");
    }
    let title: String = title.chars().take(100).collect();
    let mut description = clean(upload.description.trim());
    if upload.short && !description.contains("#Shorts") {
        description = format!("{description}\n\n#Shorts").trim().to_owned();
    }
    while description.len() > 5000 {
        description.pop();
    }
    let mut tags = Vec::new();
    let mut length = 0;
    for tag in upload
        .tags
        .iter()
        .map(|tag| clean(tag.trim().trim_start_matches('#')))
    {
        if tag.is_empty() || length + tag.chars().count() + 1 > 500 {
            continue;
        }
        length += tag.chars().count() + 1;
        tags.push(tag);
    }
    json!({
        "snippet": {
            "title": title,
            "description": description,
            "tags": tags,
            "categoryId": "27",
        },
        "status": {
            "privacyStatus": upload.privacy.word(),
            "selfDeclaredMadeForKids": false,
        }
    })
}

/// Puts the video in `file` up with `upload`'s words, through YouTube's
/// resumable upload; `progress` hears the share sent, 0 to 1. Its id.
pub fn upload(
    access: &str,
    file: &Path,
    upload: &Upload,
    progress: &mut dyn FnMut(f32),
) -> Result<String, String> {
    let bytes = std::fs::read(file).map_err(|error| format!("{}: {error}", file.display()))?;
    let session = agent()
        .post(&format!(
            "{UPLOAD}?uploadType=resumable&part=snippet,status"
        ))
        .set("authorization", &format!("Bearer {access}"))
        .set("x-upload-content-type", "video/mp4")
        .set("x-upload-content-length", &bytes.len().to_string())
        .send_json(metadata(upload))
        .map_err(|error| refused("YouTube", error))?;
    let location = session
        .header("location")
        .ok_or("YouTube gave nowhere to send the video")?
        .to_owned();
    // Sent in pieces, each a multiple of 256 KiB as YouTube asks, so the
    // bar moves and a phone does not hold one long request.
    const PIECE: usize = 8 * 1024 * 1024;
    let total = bytes.len();
    let mut sent = 0;
    loop {
        let end = (sent + PIECE).min(total);
        let answer = agent()
            .put(&location)
            .set("authorization", &format!("Bearer {access}"))
            .set(
                "content-range",
                &format!("bytes {sent}-{}/{total}", end.saturating_sub(1)),
            )
            .send_bytes(&bytes[sent..end]);
        progress(end as f32 / total.max(1) as f32);
        match answer {
            Ok(done) => {
                let video: Value = done.into_json().map_err(|error| error.to_string())?;
                return video["id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "YouTube took the video but gave no id".to_owned());
            }
            // 308: the piece is in, send the next.
            Err(ureq::Error::Status(308, _)) if end < total => sent = end,
            Err(error) => return Err(refused("YouTube", error)),
        }
    }
}

/// A video's numbers as YouTube counts them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Numbers {
    /// The video's id.
    pub id: String,
    /// Its title.
    pub title: String,
    /// Times watched.
    pub views: u64,
    /// Likes.
    pub likes: u64,
    /// Comments.
    pub comments: u64,
}

/// The numbers in YouTube's answer about some videos.
pub fn numbers_of(answer: &Value) -> Vec<Numbers> {
    let count = |value: &Value| {
        value
            .as_str()
            .and_then(|text| text.parse().ok())
            .or_else(|| value.as_u64())
            .unwrap_or(0)
    };
    answer["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| Numbers {
            id: item["id"].as_str().unwrap_or_default().to_owned(),
            title: item["snippet"]["title"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            views: count(&item["statistics"]["viewCount"]),
            likes: count(&item["statistics"]["likeCount"]),
            comments: count(&item["statistics"]["commentCount"]),
        })
        .collect()
}

/// The numbers of the videos `ids`, fifty at a time as YouTube takes them.
pub fn numbers(access: &str, ids: &[String]) -> Result<Vec<Numbers>, String> {
    let mut all = Vec::new();
    for chunk in ids.chunks(50) {
        let answer: Value = agent()
            .get(&format!(
                "{API}/videos?part=snippet,statistics&id={}",
                encoded(&chunk.join(","))
            ))
            .set("authorization", &format!("Bearer {access}"))
            .call()
            .map_err(|error| refused("YouTube", error))?
            .into_json()
            .map_err(|error| error.to_string())?;
        all.extend(numbers_of(&answer));
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PKCE's challenge is the verifier's SHA-256, base64url: RFC 7636's
    /// own example.
    #[test]
    fn the_challenge_is_rfc_7636s() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    /// The page asks for both scopes, offline, with the challenge, and
    /// sends the browser back to the port given.
    #[test]
    fn the_sign_in_page_asks_what_it_must() {
        let client = Client {
            id: "abc.apps.googleusercontent.com".into(),
            secret: "s".into(),
        };
        let url = auth_url(&client, "http://127.0.0.1:5000", "st", "verifier");
        assert!(url.starts_with(AUTH));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000"));
        assert!(url.contains("youtube.upload"));
        assert!(url.contains("youtube.readonly"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains(&format!("code_challenge={}", challenge("verifier"))));
        assert!(!Client::default().is_set());
    }

    /// The code is read from the browser's request when its state is ours;
    /// another state, an error, or no code is said.
    #[test]
    fn the_code_is_read_from_the_way_back() {
        assert_eq!(
            code_of("GET /?state=st&code=4%2F0Ab HTTP/1.1", "st").as_deref(),
            Ok("4/0Ab")
        );
        assert!(code_of("GET /?state=other&code=x HTTP/1.1", "st").is_err());
        assert!(
            code_of("GET /?error=access_denied&state=st HTTP/1.1", "st")
                .unwrap_err()
                .contains("access_denied")
        );
        assert!(code_of("GET /?state=st HTTP/1.1", "st").is_err());
        assert!(code_of("", "st").is_err());
    }

    /// Two random strings differ, and are as long as asked.
    #[test]
    fn random_strings_differ() {
        let (a, b) = (random_hex(16), random_hex(16));
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }

    /// The words go up within YouTube's limits, a Short marked so.
    #[test]
    fn the_metadata_keeps_to_youtubes_limits() {
        let upload = Upload {
            title: "A <bold> title".repeat(20),
            description: "Why.".into(),
            tags: vec![
                "#history".into(),
                "x".repeat(600),
                " ".into(),
                "rome".into(),
            ],
            privacy: Privacy::Unlisted,
            short: true,
        };
        let meta = metadata(&upload);
        let title = meta["snippet"]["title"].as_str().unwrap();
        assert!(title.chars().count() <= 100);
        assert!(!title.contains('<'));
        assert!(
            meta["snippet"]["description"]
                .as_str()
                .unwrap()
                .ends_with("#Shorts")
        );
        assert_eq!(meta["snippet"]["tags"], json!(["history", "rome"]));
        assert_eq!(meta["status"]["privacyStatus"], "unlisted");
        assert_eq!(Privacy::of_row(9), Privacy::Private);
    }

    /// YouTube's counts, given as text, are read as numbers.
    #[test]
    fn numbers_are_read() {
        let answer = json!({"items": [{"id": "v1", "snippet": {"title": "One"},
            "statistics": {"viewCount": "1234", "likeCount": "56"}}]});
        let read = numbers_of(&answer);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].views, 1234);
        assert_eq!(read[0].likes, 56);
        assert_eq!(read[0].comments, 0);
    }
}
