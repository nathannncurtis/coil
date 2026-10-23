//! Unboxing: values inference has narrowed lose their `Box`/`Unbox` pairs and
//! run as unboxed arithmetic, overflow-checked and boxing to `BigInt` on
//! overflow (spec sections 5 and 7).
