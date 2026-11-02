//! Request-local resource accounting, separate from symbol and record lowering.
use crate::{
    FrontendLimits,
    diagnostic::{Result, fail},
};

pub(super) struct FrontendBudget<'a> {
    limits: &'a FrontendLimits,
    work: usize,
    depth: usize,
    intermediate_bytes: usize,
}
impl<'a> FrontendBudget<'a> {
    pub fn new(limits: &'a FrontendLimits) -> Self {
        Self {
            limits,
            work: 0,
            depth: 0,
            intermediate_bytes: 0,
        }
    }
    pub fn charge(&mut self, amount: usize) -> Result<()> {
        self.work = self
            .work
            .checked_add(amount)
            .ok_or_else(|| fail("C_SOURCE_LIMIT", "work overflow"))?;
        if self.work > self.limits.max_work {
            return Err(fail("C_SOURCE_LIMIT", "frontend work limit exceeded"));
        }
        Ok(())
    }
    pub fn account(&mut self, bytes: usize) -> Result<()> {
        self.intermediate_bytes = self
            .intermediate_bytes
            .checked_add(bytes)
            .ok_or_else(|| fail("C_SOURCE_LIMIT", "intermediate size overflow"))?;
        if self.intermediate_bytes > self.limits.max_intermediate_bytes {
            return Err(fail(
                "C_SOURCE_LIMIT",
                "intermediate construction byte limit exceeded",
            ));
        }
        Ok(())
    }
    pub fn enter(&mut self) -> Result<()> {
        self.charge(1)?;
        if self.depth >= self.limits.max_depth {
            return Err(fail("C_SOURCE_LIMIT", "resolution nesting limit exceeded"));
        }
        self.depth += 1;
        Ok(())
    }
    pub fn leave(&mut self) {
        self.depth -= 1;
    }
}
