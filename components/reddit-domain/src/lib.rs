use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::collections::HashMap;

mod bindings;

#[derive(Clone)]
pub struct Subreddit {
    id: String,
    name: String,
}

#[derive(Clone)]
pub struct Thread {
    id: String,
    subreddit_id: String,
    title: String,
    content: String,
    upvotes: i32,
}

#[derive(Clone)]
pub struct Comment {
    id: String,
    thread_id: String,
    content: String,
    upvotes: i32,
}

struct State {
    subreddits: HashMap<String, Subreddit>,
    threads: HashMap<String, Thread>,
    comments: HashMap<String, Comment>,
    next_id: usize,
}

impl State {
    fn new() -> Self {
        Self {
            subreddits: HashMap::new(),
            threads: HashMap::new(),
            comments: HashMap::new(),
            next_id: 1,
        }
    }
    fn get_id(&mut self) -> String {
        let id = self.next_id.to_string();
        self.next_id += 1;
        id
    }
}

static STATE: Lazy<Mutex<State>> = Lazy::new(|| Mutex::new(State::new()));

struct RedditDomain;

impl bindings::exports::local::reddit::reddit::Guest for RedditDomain {
    fn create_subreddit(name: String) -> String {
        let mut state = STATE.lock().unwrap();
        let id = state.get_id();
        state.subreddits.insert(id.clone(), Subreddit {
            id: id.clone(),
            name,
        });
        id
    }

    fn get_subreddits() -> Vec<bindings::exports::local::reddit::reddit::Subreddit> {
        let state = STATE.lock().unwrap();
        state.subreddits.values().map(|s| bindings::exports::local::reddit::reddit::Subreddit {
            id: s.id.clone(),
            name: s.name.clone(),
        }).collect()
    }

    fn create_thread(subreddit_id: String, title: String, content: String) -> String {
        let mut state = STATE.lock().unwrap();
        let id = state.get_id();
        state.threads.insert(id.clone(), Thread {
            id: id.clone(),
            subreddit_id,
            title,
            content,
            upvotes: 0,
        });
        id
    }

    fn get_threads(subreddit_id: String) -> Vec<bindings::exports::local::reddit::reddit::Thread> {
        let state = STATE.lock().unwrap();
        state.threads.values()
            .filter(|t| t.subreddit_id == subreddit_id)
            .map(|t| bindings::exports::local::reddit::reddit::Thread {
                id: t.id.clone(),
                subreddit_id: t.subreddit_id.clone(),
                title: t.title.clone(),
                content: t.content.clone(),
                upvotes: t.upvotes,
            }).collect()
    }

    fn upvote_thread(thread_id: String) {
        let mut state = STATE.lock().unwrap();
        if let Some(thread) = state.threads.get_mut(&thread_id) {
            thread.upvotes += 1;
        }
    }

    fn downvote_thread(thread_id: String) {
        let mut state = STATE.lock().unwrap();
        if let Some(thread) = state.threads.get_mut(&thread_id) {
            thread.upvotes -= 1;
        }
    }

    fn create_comment(thread_id: String, content: String) -> String {
        let mut state = STATE.lock().unwrap();
        let id = state.get_id();
        state.comments.insert(id.clone(), Comment {
            id: id.clone(),
            thread_id,
            content,
            upvotes: 0,
        });
        id
    }

    fn get_comments(thread_id: String) -> Vec<bindings::exports::local::reddit::reddit::Comment> {
        let state = STATE.lock().unwrap();
        state.comments.values()
            .filter(|c| c.thread_id == thread_id)
            .map(|c| bindings::exports::local::reddit::reddit::Comment {
                id: c.id.clone(),
                thread_id: c.thread_id.clone(),
                content: c.content.clone(),
                upvotes: c.upvotes,
            }).collect()
    }

    fn upvote_comment(comment_id: String) {
        let mut state = STATE.lock().unwrap();
        if let Some(comment) = state.comments.get_mut(&comment_id) {
            comment.upvotes += 1;
        }
    }

    fn downvote_comment(comment_id: String) {
        let mut state = STATE.lock().unwrap();
        if let Some(comment) = state.comments.get_mut(&comment_id) {
            comment.upvotes -= 1;
        }
    }
}

bindings::export!(RedditDomain with_types_in bindings);
