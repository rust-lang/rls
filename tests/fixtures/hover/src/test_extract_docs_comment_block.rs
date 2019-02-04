////////////////////////////////////////////////////////////////////////////////
// Free functions
////////////////////////////////////////////////////////////////////////////////

/// The standard library often has comment header blocks that should not be
/// included.
/// 
/// Nam efficitur dapibus lectus consequat porta. Pellentesque augue metus,
/// vestibulum nec massa at, aliquet consequat ex.
/// 
/// End of spawn docs
pub fn spawn<F, T>(_f: F) -> JoinHandle<T> where
    F: FnOnce() -> T, F: Send + 'static, T: Send + 'static
{
    unimplemented!()
}