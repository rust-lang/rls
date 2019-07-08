#[cfg(unix)]
pub fn get_resident() -> Option<usize> {
    use std::fs::File;
    use std::io::Read;

    let field = 1;
    let mut f = File::open("/proc/self/statm").ok()?;
    let mut contents = String::new();
    f.read_to_string(&mut contents).ok()?;
    let s = contents.split_whitespace().nth(field)?;
    let npages = s.parse::<usize>().ok()?;
    Some(npages * 4096)
}

#[cfg(not(unix))]
pub fn get_resident() -> Option<usize> {
    None
}
