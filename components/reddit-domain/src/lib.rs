//! `reddit-domain` — run threaded discussion boards where people post and reply under topics

use bindings::auth::identity::authorizer::authorize;
use bindings::auth::identity::types::Permission;
use bindings::wasi::keyvalue::store::open;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

mod bindings;

#[derive(Clone, Serialize, Deserialize)]
pub struct Subreddit {
    id: String,
    name: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Thread {
    id: String,
    subreddit_id: String,
    title: String,
    content: String,
    upvotes: i32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Comment {
    id: String,
    thread_id: String,
    content: String,
    upvotes: i32,
}

struct State {
    next_id: usize,
}

impl State {
    fn new() -> Self {
        Self { next_id: 1 }
    }
    fn get_id(&mut self) -> String {
        let id = self.next_id.to_string();
        self.next_id += 1;
        id
    }
}

static STATE: Lazy<Mutex<State>> = Lazy::new(|| Mutex::new(State::new()));

struct RedditDomain;

impl RedditDomain {
    fn check_auth(token: &str, target: &str, action: &str) {
        let perm = Permission { target: target.to_string(), action: action.to_string() };
        let _principal = authorize(token, &perm).unwrap();
    }

    fn get_bucket() -> bindings::wasi::keyvalue::store::Bucket {
        open("default").unwrap()
    }

    fn load_subreddits() -> Vec<Subreddit> {
        let bucket = Self::get_bucket();
        if let Ok(Some(bytes)) = bucket.get("subreddits") {
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    fn save_subreddits(subreddits: &Vec<Subreddit>) {
        let bucket = Self::get_bucket();
        let bytes = serde_json::to_vec(subreddits).unwrap();
        bucket.set("subreddits", &bytes).unwrap();
    }

    fn load_threads() -> Vec<Thread> {
        let bucket = Self::get_bucket();
        if let Ok(Some(bytes)) = bucket.get("threads") {
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    fn save_threads(threads: &Vec<Thread>) {
        let bucket = Self::get_bucket();
        let bytes = serde_json::to_vec(threads).unwrap();
        bucket.set("threads", &bytes).unwrap();
    }

    fn load_comments() -> Vec<Comment> {
        let bucket = Self::get_bucket();
        if let Ok(Some(bytes)) = bucket.get("comments") {
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    fn save_comments(comments: &Vec<Comment>) {
        let bucket = Self::get_bucket();
        let bytes = serde_json::to_vec(comments).unwrap();
        bucket.set("comments", &bytes).unwrap();
    }
}

impl bindings::exports::local::reddit::reddit::Guest for RedditDomain {
    fn create_subreddit(name: String, token: String) -> String {
        Self::check_auth(&token, "subreddit", "create");
        let mut state = STATE.lock().unwrap();
        let id = state.get_id();
        let mut subreddits = Self::load_subreddits();
        subreddits.push(Subreddit { id: id.clone(), name });
        Self::save_subreddits(&subreddits);
        id
    }

    fn get_subreddits() -> Vec<bindings::exports::local::reddit::reddit::Subreddit> {
        Self::load_subreddits()
            .into_iter()
            .map(|s| bindings::exports::local::reddit::reddit::Subreddit { id: s.id, name: s.name })
            .collect()
    }

    fn create_thread(
        subreddit_id: String,
        title: String,
        content: String,
        token: String,
    ) -> String {
        Self::check_auth(&token, "thread", "create");
        let mut state = STATE.lock().unwrap();
        let id = state.get_id();
        let mut threads = Self::load_threads();
        threads.push(Thread { id: id.clone(), subreddit_id, title, content, upvotes: 0 });
        Self::save_threads(&threads);
        id
    }

    fn get_threads(subreddit_id: String) -> Vec<bindings::exports::local::reddit::reddit::Thread> {
        Self::load_threads()
            .into_iter()
            .filter(|t| t.subreddit_id == subreddit_id)
            .map(|t| bindings::exports::local::reddit::reddit::Thread {
                id: t.id,
                subreddit_id: t.subreddit_id,
                title: t.title,
                content: t.content,
                upvotes: t.upvotes,
            })
            .collect()
    }

    fn upvote_thread(thread_id: String, token: String) {
        Self::check_auth(&token, "thread", "upvote");
        let mut threads = Self::load_threads();
        if let Some(thread) = threads.iter_mut().find(|t| t.id == thread_id) {
            thread.upvotes += 1;
            Self::save_threads(&threads);
        }
    }

    fn downvote_thread(thread_id: String, token: String) {
        Self::check_auth(&token, "thread", "downvote");
        let mut threads = Self::load_threads();
        if let Some(thread) = threads.iter_mut().find(|t| t.id == thread_id) {
            thread.upvotes -= 1;
            Self::save_threads(&threads);
        }
    }

    fn create_comment(thread_id: String, content: String, token: String) -> String {
        Self::check_auth(&token, "comment", "create");
        let mut state = STATE.lock().unwrap();
        let id = state.get_id();
        let mut comments = Self::load_comments();
        comments.push(Comment { id: id.clone(), thread_id, content, upvotes: 0 });
        Self::save_comments(&comments);
        id
    }

    fn get_comments(thread_id: String) -> Vec<bindings::exports::local::reddit::reddit::Comment> {
        Self::load_comments()
            .into_iter()
            .filter(|c| c.thread_id == thread_id)
            .map(|c| bindings::exports::local::reddit::reddit::Comment {
                id: c.id,
                thread_id: c.thread_id,
                content: c.content,
                upvotes: c.upvotes,
            })
            .collect()
    }

    fn upvote_comment(comment_id: String, token: String) {
        Self::check_auth(&token, "comment", "upvote");
        let mut comments = Self::load_comments();
        if let Some(comment) = comments.iter_mut().find(|c| c.id == comment_id) {
            comment.upvotes += 1;
            Self::save_comments(&comments);
        }
    }

    fn downvote_comment(comment_id: String, token: String) {
        Self::check_auth(&token, "comment", "downvote");
        let mut comments = Self::load_comments();
        if let Some(comment) = comments.iter_mut().find(|c| c.id == comment_id) {
            comment.upvotes -= 1;
            Self::save_comments(&comments);
        }
    }
}

bindings::export!(RedditDomain with_types_in bindings);
