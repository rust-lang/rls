use fst::{self, Streamer};

/// `SymbolQuery` specifies the preficate for filtering symbols by name.
///
/// All matching is case-insensitive. Filtering by prefix or by subsequence
/// is supported, subsequence being a good default choice.
///
/// As the number of results might be huge, consider the `limit` hint,
/// which serves as *approximate* limit on the number of results returned.
///
/// To implement async streaming/pagination, use `greater_than` together with
/// `limit`.
#[derive(Debug)]
pub struct SymbolQuery {
    query_string: String,
    mode: Mode,
    limit: usize,
    greater_than: String,
}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Prefix,
    Subsequence,
}

impl SymbolQuery {
    fn new(query_string: String, mode: Mode) -> SymbolQuery {
        SymbolQuery { query_string, mode, limit: usize::max_value(), greater_than: String::new() }
    }

    pub fn subsequence(query_string: &str) -> SymbolQuery {
        SymbolQuery::new(query_string.to_lowercase(), Mode::Subsequence)
    }

    pub fn prefix(query_string: &str) -> SymbolQuery {
        SymbolQuery::new(query_string.to_lowercase(), Mode::Prefix)
    }

    pub fn limit(self, limit: usize) -> SymbolQuery {
        SymbolQuery { limit, ..self }
    }

    pub fn greater_than(self, greater_than: &str) -> SymbolQuery {
        SymbolQuery { greater_than: greater_than.to_lowercase(), ..self }
    }

    pub(crate) fn build_stream<'a, I>(&'a self, fsts: I) -> fst::map::Union<'a>
    where
        I: Iterator<Item = &'a fst::Map<Vec<u8>>>,
    {
        let mut stream = fst::map::OpBuilder::new();
        let automaton = QueryAutomaton { query: &self.query_string, mode: self.mode };
        for fst in fsts {
            stream = stream.add(fst.search(automaton).gt(&self.greater_than));
        }
        stream.union()
    }

    pub(crate) fn search_stream<F, T>(&self, mut stream: fst::map::Union<'_>, f: F) -> Vec<T>
    where
        F: Fn(&mut Vec<T>, &fst::map::IndexedValue),
    {
        let mut res = Vec::new();
        while let Some((_, entries)) = stream.next() {
            for e in entries {
                f(&mut res, e);
            }
            if res.len() >= self.limit {
                break;
            }
        }
        res
    }
}

/// See http://docs.rs/fst for how we implement query processing.
///
/// In a nutshell, both the query and the set of available symbols
/// are encoded as two finite state machines. Then, the intersection
/// of state machines is built, which gives all the symbols matching
/// the query.
///
/// The `fst::Automaton` impl below implements a state machine for
/// the query, where the state is the number of bytes already matched.
#[derive(Clone, Copy)]
struct QueryAutomaton<'a> {
    query: &'a str,
    mode: Mode,
}

const NO_MATCH: usize = !0;

impl<'a> fst::Automaton for QueryAutomaton<'a> {
    type State = usize;

    fn start(&self) -> usize {
        0
    }

    fn is_match(&self, &state: &usize) -> bool {
        state == self.query.len()
    }

    fn accept(&self, &state: &usize, byte: u8) -> usize {
        if state == NO_MATCH {
            return state;
        }
        if state == self.query.len() {
            return state;
        }
        if byte == self.query.as_bytes()[state] {
            return state + 1;
        }
        match self.mode {
            Mode::Prefix => NO_MATCH,
            Mode::Subsequence => state,
        }
    }

    fn can_match(&self, &state: &usize) -> bool {
        state != NO_MATCH
    }

    fn will_always_match(&self, &state: &usize) -> bool {
        state == self.query.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::iter;

    const STARS: &[&str] = &[
        "agena", "agreetor", "algerib", "anektor", "antares", "arcturus", "canopus", "capella",
        "duendin", "golubin", "lalandry", "spica", "vega",
    ];

    fn check(q: SymbolQuery, expected: &[&str]) {
        let map =
            fst::Map::from_iter(STARS.iter().enumerate().map(|(i, &s)| (s, i as u64))).unwrap();
        let stream = q.build_stream(iter::once(&map));
        let actual = q.search_stream(stream, |acc, iv| acc.push(STARS[iv.value as usize]));
        assert_eq!(expected, actual.as_slice());
    }

    #[test]
    fn test_automaton() {
        check(SymbolQuery::prefix("an"), &["anektor", "antares"]);

        check(
            SymbolQuery::subsequence("an"),
            &["agena", "anektor", "antares", "canopus", "lalandry"],
        );

        check(SymbolQuery::subsequence("an").limit(2), &["agena", "anektor"]);
        check(
            SymbolQuery::subsequence("an").limit(2).greater_than("anektor"),
            &["antares", "canopus"],
        );
        check(SymbolQuery::subsequence("an").limit(2).greater_than("canopus"), &["lalandry"]);
    }
}
