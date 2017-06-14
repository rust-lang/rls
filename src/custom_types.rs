use lsp_data::Range;
use lsp_data::ls_util::rls_to_range;
use std::convert::From;
use analysis;

#[derive(Debug, Serialize, Deserialize)]
pub struct Borrows {
    pub assignments: Vec<Assignment>,
    pub loans: Vec<Loan>,
    pub moves: Vec<Move>,
}

impl From<analysis::Borrows> for Borrows {
    fn from(borrows: analysis::Borrows) -> Borrows {
        Borrows {
            assignments: borrows.assignments.into_iter().map(|a| a.into()).collect(),
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
pub struct Assignment {
    pub range: Range,
}

impl From<analysis::Assignment> for Assignment {
    fn from(assignment: analysis::Assignment) -> Assignment {
        Assignment {
            range: rls_to_range(assignment.span.range),
        }
    }
}