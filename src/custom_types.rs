use lsp_data::Range;
use lsp_data::ls_util::rls_to_range;
use std::convert::From;
use analysis;

#[derive(Debug, Serialize, Deserialize)]
pub struct BorrowData {
    pub scopes: Vec<Scope>,
    pub loans: Vec<Loan>,
    pub moves: Vec<Move>,
}

impl From<analysis::BorrowData> for BorrowData {
    fn from(borrows: analysis::BorrowData) -> BorrowData {
        BorrowData {
            scopes: borrows.scopes.into_iter().map(|a| a.into()).collect(),
            moves: borrows.moves.into_iter().map(|m| m.into()).collect(),
            loans: borrows.loans.into_iter().map(|l| l.into()).collect(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum BorrowKind {
    ImmBorrow,
    MutBorrow,
}

impl From<analysis::BorrowKind> for BorrowKind {
    fn from(kind: analysis::BorrowKind) -> BorrowKind {
        match kind {
            analysis::BorrowKind::ImmBorrow => BorrowKind::ImmBorrow,
            analysis::BorrowKind::MutBorrow => BorrowKind::MutBorrow,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Loan {
    pub kind: BorrowKind,
    pub range: Range,
}

impl From<analysis::Loan> for Loan {
    fn from(loan: analysis::Loan) -> Loan {
        Loan {
            kind: loan.kind.into(),
            range: rls_to_range(loan.span.range),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Move {
    pub range: Range,
}

impl From<analysis::Move> for Move {
    fn from(mov: analysis::Move) -> Move {
        Move {
            range: rls_to_range(mov.span.range),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Scope {
    pub range: Range,
}

impl From<analysis::Scope> for Scope {
    fn from(scope: analysis::Scope) -> Scope {
        Scope {
            range: rls_to_range(scope.span.range),
        }
    }
}