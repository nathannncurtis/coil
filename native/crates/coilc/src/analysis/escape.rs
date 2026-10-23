//! Escape analysis: allocations that do not outlive their frame and never
//! reach boxed or C code become `StackAlloc` or are scalar-replaced
//! (spec section 7).
