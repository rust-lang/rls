#![allow(dead_code)]

#[derive(PartialEq)]
struct Bar;
struct Foo;

trait Super{}
trait Sub: Super {}

impl Super for Bar {}
impl Eq for Bar {}

impl Sub for Foo {}
impl Super for Foo {}
