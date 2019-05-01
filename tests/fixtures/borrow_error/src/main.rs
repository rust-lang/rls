fn main() {
    let mut x = 3;
    let y = &mut x;
    let z = &mut x;
    *y += 1;
}
