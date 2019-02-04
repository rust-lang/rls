// Spot check several stdlib items and verify that the the doc_url
// is correctly included for traits. The tests around the stdlib 
// are subject to breakage due to changes in docs, so these tests
// are not very comprehensive.


fn test() {
    let mut vec1 = Vec::new();
    vec1.push(1);
    let slice = &vec1[0..];
    let _vec2 = vec1.clone();
    let _vec3 = Vec::<u32>::default();
    let _one = slice[0];
    let _one_ref = &slice[0];
    use std::string::ToString;
}