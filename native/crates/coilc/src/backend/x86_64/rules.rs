//! Matching rules from machine-shaped IR to x86-64 instruction forms
//! (`IAddOvf -> add; jo`, `LoadField -> mov reg,[base+off]`)
//! (spec section 12.1).
