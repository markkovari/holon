struct Page {
    hits: Vec<String>,
    has_more: bool,
}

pub fn paginate(ids: Vec<String>, size: u32, offset: u32) -> Page {
    let offset = offset as usize;
    let size = size as usize;
    
    // Clamp offset to valid range
    let start = offset.min(ids.len());
    
    // Get the slice for this page
    let hits: Vec<String> = ids[start..]
        .iter()
        .take(size)
        .cloned()
        .collect();
    
    // Check if there are more results after this page
    let has_more = start + size < ids.len();
    
    Page { hits, has_more }
}