/// One page of results.
pub struct Page {
    /// The ids on this page.
    pub hits: Vec<String>,
    /// Whether another page exists.
    pub has_more: bool,
}

/// Page a list of ids.
pub fn paginate(ids: Vec<String>, size: u32, offset: u32) -> Page {
    let offset = offset as usize;
    let size = size as usize;
    
    let start = offset;
    let end = (offset + size).min(ids.len());
    
    let hits = if start < ids.len() {
        ids[start..end].to_vec()
    } else {
        vec![]
    };
    
    let has_more = end < ids.len();
    
    Page { hits, has_more }
}