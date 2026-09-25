//! Numeric primitives + the full numeric tower.
//! Ports `src/prim/numeric.c`, `numeric_bit.c`, `numeric_coerce.c`,
//! `bignum.c`, `ratio.c`.
//!
//! Tier order (contagion): Int < BigInt < Ratio < Float. (BigDec is deferred;
//! arithmetic_test/clj_math don't use `M` literals.) `+ - *` fold from the
//! first operand; the accumulator promotes one-way as higher-tier operands
//! arrive. Int overflow in the STRICT forms (`+ - * inc dec`) throws
//! `MCT001 integer overflow`; the PRIMED forms (`+' -' *' inc' dec'`)
//! auto-promote Int -> BigInt instead. `/` is exact: int/int that divides
//! evenly stays Int, else a reduced Ratio. Any Float operand collapses the
//! result to Float (f64). Ratios always reduce; one that reduces to an
//! integer becomes Int/BigInt, never a Ratio.

use crate::error::{throw_classified, Throw};
use crate::eval::Interp;
use crate::value::{BigIntVal, RatioVal, Value};
use gc::Gc;
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, Signed, ToPrimitive, Zero};

fn throw_overflow() -> Throw {
    throw_classified("eval/contract", "MCT001", "integer overflow")
}
fn throw_type(msg: &str) -> Throw {
    throw_classified("eval/type", "MTY001", msg)
}

// The i64 range as f64 bounds. i64::MAX rounds up to 2^63 as an f64, so a
// double >= 2^63 or < -2^63 is out of long range. (Exact-digit f64 literals
// tripped clippy's excessive-precision lint; powers of two are exact.)
const LONG_MAX_F64: f64 = 9_223_372_036_854_775_808.0; // 2^63
const LONG_MIN_F64: f64 = -9_223_372_036_854_775_808.0; // -2^63

// ---------------------------------------------------------------------------
// Tower value + tier machinery
// ---------------------------------------------------------------------------

/// A numeric operand lifted out of a `Value`. Float carries both the f64 and
/// whether the source was a 32-bit float32 (contagion still collapses to f64
/// arithmetic, but a lone float32 operand's tier is Float).
#[derive(Clone)]
enum Num {
    Int(i64),
    Big(BigInt),
    Ratio(BigRational),
    Float(f64),
}

impl Num {
    fn tier(&self) -> u8 {
        match self {
            Num::Int(_) => 0,
            Num::Big(_) => 1,
            Num::Ratio(_) => 2,
            Num::Float(_) => 3,
        }
    }
    fn to_f64(&self) -> f64 {
        match self {
            Num::Int(n) => *n as f64,
            Num::Big(b) => b.to_f64().unwrap_or(f64::NAN),
            Num::Ratio(r) => r.to_f64().unwrap_or(f64::NAN),
            Num::Float(x) => *x,
        }
    }
    fn to_big(&self) -> BigInt {
        match self {
            Num::Int(n) => BigInt::from(*n),
            Num::Big(b) => b.clone(),
            _ => unreachable!("to_big on non-integer tier"),
        }
    }
    fn to_ratio(&self) -> BigRational {
        match self {
            Num::Int(n) => BigRational::from(BigInt::from(*n)),
            Num::Big(b) => BigRational::from(b.clone()),
            Num::Ratio(r) => r.clone(),
            _ => unreachable!("to_ratio on float tier"),
        }
    }
}

fn as_num(v: &Value, op: &str) -> Result<Num, Throw> {
    match v {
        Value::Int(n) => Ok(Num::Int(*n)),
        Value::BigInt(b) => Ok(Num::Big(b.0.clone())),
        Value::Ratio(r) => Ok(Num::Ratio(r.0.clone())),
        Value::Float(x) => Ok(Num::Float(*x)),
        Value::Float32(x) => Ok(Num::Float(*x as f64)),
        other => Err(throw_type(&format!(
            "{op} expects numbers, got {}",
            crate::printer::print_str(other)
        ))),
    }
}

/// Pack a `BigInt` result into the tightest tier: Int when it fits i64, else
/// BigInt. (mino keeps bigint-tier results in bigint per contagion, but a
/// fresh integer that never went through the bigint tier narrows; the fold
/// below tracks whether the accumulator ever promoted.)
fn big_to_value_narrow(b: BigInt) -> Value {
    match b.to_i64() {
        Some(n) => Value::Int(n),
        None => Value::BigInt(Gc::new(BigIntVal(b))),
    }
}

/// A BigInt result that MUST stay in the bigint tier (contagion): keep as
/// BigInt even when it fits i64.
fn big_to_value_keep(b: BigInt) -> Value {
    Value::BigInt(Gc::new(BigIntVal(b)))
}

/// Pack a reduced rational: if the denominator is 1 it collapses to Int/BigInt,
/// else a Ratio.
fn ratio_to_value(r: BigRational) -> Value {
    if r.denom().is_one() {
        big_to_value_narrow(r.numer().clone())
    } else {
        Value::Ratio(Gc::new(RatioVal(r)))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

/// The tower accumulator. `promoted` records whether we ever left the Int
/// tier into Big/Ratio, so an integer-valued result stays bigint-tier per
/// contagion (matching mino's `bigint_or_self`).
struct Acc {
    tier: u8,
    iacc: i64,
    big: BigInt,
    ratio: BigRational,
    facc: f64,
    promoted: bool,
}

impl Acc {
    fn seed(n: &Num) -> Acc {
        let mut a = Acc {
            tier: n.tier(),
            iacc: 0,
            big: BigInt::zero(),
            ratio: BigRational::zero(),
            facc: 0.0,
            promoted: n.tier() > 0,
        };
        match n {
            Num::Int(v) => a.iacc = *v,
            Num::Big(b) => a.big = b.clone(),
            Num::Ratio(r) => a.ratio = r.clone(),
            Num::Float(x) => a.facc = *x,
        }
        a
    }

    fn promote_to(&mut self, tier: u8) {
        while self.tier < tier {
            match self.tier {
                0 => match tier {
                    1 => {
                        self.big = BigInt::from(self.iacc);
                        self.tier = 1;
                        self.promoted = true;
                    }
                    2 => {
                        self.ratio = BigRational::from(BigInt::from(self.iacc));
                        self.tier = 2;
                        self.promoted = true;
                    }
                    _ => {
                        self.facc = self.iacc as f64;
                        self.tier = 3;
                    }
                },
                1 => match tier {
                    2 => {
                        self.ratio = BigRational::from(self.big.clone());
                        self.tier = 2;
                    }
                    _ => {
                        self.facc = self.big.to_f64().unwrap_or(f64::NAN);
                        self.tier = 3;
                    }
                },
                2 => {
                    self.facc = self.ratio.to_f64().unwrap_or(f64::NAN);
                    self.tier = 3;
                }
                _ => break,
            }
        }
    }

    /// Apply one operand. `strict` controls Int-tier overflow behavior.
    fn apply(&mut self, n: &Num, op: Op, strict: bool) -> Result<(), Throw> {
        // Promote to the max of the current tier and the operand's tier.
        let want = self.tier.max(n.tier());
        if want > self.tier {
            self.promote_to(want);
        }
        match self.tier {
            0 => self.apply_int(n, op, strict),
            1 => self.apply_big(n, op),
            2 => self.apply_ratio(n, op),
            _ => {
                let x = n.to_f64();
                match op {
                    Op::Add => self.facc += x,
                    Op::Sub => self.facc -= x,
                    Op::Mul => self.facc *= x,
                    Op::Div => self.facc /= x,
                }
                Ok(())
            }
        }
    }

    fn apply_int(&mut self, n: &Num, op: Op, strict: bool) -> Result<(), Throw> {
        let x = match n {
            Num::Int(v) => *v,
            _ => unreachable!(),
        };
        match op {
            Op::Add | Op::Sub | Op::Mul => {
                let checked = match op {
                    Op::Add => self.iacc.checked_add(x),
                    Op::Sub => self.iacc.checked_sub(x),
                    _ => self.iacc.checked_mul(x),
                };
                match checked {
                    Some(v) => {
                        self.iacc = v;
                        Ok(())
                    }
                    None if strict => Err(throw_overflow()),
                    None => {
                        // Primed: promote to bigint and redo this step there.
                        let a = BigInt::from(self.iacc);
                        let b = BigInt::from(x);
                        self.big = match op {
                            Op::Add => a + b,
                            Op::Sub => a - b,
                            _ => a * b,
                        };
                        self.tier = 1;
                        self.promoted = true;
                        Ok(())
                    }
                }
            }
            Op::Div => {
                if x == 0 {
                    return Err(throw_type("division by zero"));
                }
                if self.iacc % x == 0 {
                    self.iacc /= x;
                    Ok(())
                } else {
                    // Non-exact int/int -> reduced ratio.
                    let r = BigRational::new(BigInt::from(self.iacc), BigInt::from(x));
                    self.set_from_ratio(r);
                    Ok(())
                }
            }
        }
    }

    fn apply_big(&mut self, n: &Num, op: Op) -> Result<(), Throw> {
        let b = n.to_big();
        if op == Op::Div {
            // bigint/bigint -> ratio (may collapse back to int/bigint).
            if b.is_zero() {
                return Err(throw_type("division by zero"));
            }
            let r = BigRational::new(self.big.clone(), b);
            self.set_from_ratio(r);
            return Ok(());
        }
        let a = std::mem::replace(&mut self.big, BigInt::zero());
        self.big = match op {
            Op::Add => a + b,
            Op::Sub => a - b,
            _ => a * b,
        };
        Ok(())
    }

    fn apply_ratio(&mut self, n: &Num, op: Op) -> Result<(), Throw> {
        let b = n.to_ratio();
        if op == Op::Div && b.is_zero() {
            return Err(throw_type("division by zero"));
        }
        let a = std::mem::replace(&mut self.ratio, BigRational::zero());
        let r = match op {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            Op::Div => a / b,
        };
        self.set_from_ratio(r);
        Ok(())
    }

    /// Store a rational result. mino's `tower_apply_ratio`/`tower_apply_bigint`
    /// collapse an integer-valued result back to the Int tier when it fits
    /// i64, else BigInt (so `(+ 1/2 1/2)` -> `1`, not `1N`). A true fraction
    /// stays a Ratio.
    fn set_from_ratio(&mut self, r: BigRational) {
        if r.denom().is_one() {
            match r.numer().to_i64() {
                Some(n) => {
                    self.iacc = n;
                    self.tier = 0;
                }
                None => {
                    self.big = r.numer().clone();
                    self.tier = 1;
                    self.promoted = true;
                }
            }
        } else {
            self.ratio = r;
            self.tier = 2;
            self.promoted = true;
        }
    }

    fn finish(self) -> Value {
        match self.tier {
            0 => Value::Int(self.iacc),
            1 => {
                if self.promoted {
                    big_to_value_keep(self.big)
                } else {
                    big_to_value_narrow(self.big)
                }
            }
            2 => ratio_to_value(self.ratio),
            _ => Value::Float(self.facc),
        }
    }
}

/// Fold `+`/`*` (seed from the first operand, identity when empty).
fn fold(args: &[Value], op: Op, ident: i64, name: &str, strict: bool) -> Result<Value, Throw> {
    if args.is_empty() {
        return Ok(Value::Int(ident));
    }
    let mut acc = Acc::seed(&as_num(&args[0], name)?);
    for v in &args[1..] {
        acc.apply(&as_num(v, name)?, op, strict)?;
    }
    Ok(acc.finish())
}

/// `-`/`-'`: unary negates, binary+ folds subtraction.
fn sub_impl(args: &[Value], name: &str, strict: bool) -> Result<Value, Throw> {
    if args.is_empty() {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            &format!("{name} requires at least one argument"),
        ));
    }
    if args.len() == 1 {
        return negate(&as_num(&args[0], name)?, strict);
    }
    let mut acc = Acc::seed(&as_num(&args[0], name)?);
    for v in &args[1..] {
        acc.apply(&as_num(v, name)?, Op::Sub, strict)?;
    }
    Ok(acc.finish())
}

fn negate(n: &Num, strict: bool) -> Result<Value, Throw> {
    match n {
        Num::Int(v) => match v.checked_neg() {
            Some(r) => Ok(Value::Int(r)),
            None if strict => Err(throw_overflow()),
            None => Ok(big_to_value_keep(-BigInt::from(*v))),
        },
        Num::Big(b) => Ok(big_to_value_keep(-b)),
        Num::Ratio(r) => Ok(ratio_to_value(-r)),
        Num::Float(x) => Ok(Value::Float(-x)),
    }
}

// ---------------------------------------------------------------------------
// + - * / (strict) and +' -' *' (primed)
// ---------------------------------------------------------------------------

pub fn add(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    fold(args, Op::Add, 0, "+", true)
}
pub fn addp(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    fold(args, Op::Add, 0, "+'", false)
}
pub fn mul(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    fold(args, Op::Mul, 1, "*", true)
}
pub fn mulp(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    fold(args, Op::Mul, 1, "*'", false)
}
pub fn sub(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    sub_impl(args, "-", true)
}
pub fn subp(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    sub_impl(args, "-'", false)
}

/// `/` — exact division (never a strict-overflow throw; ratios/bigints hold
/// any magnitude). `(/ x)` is `(/ 1 x)`.
pub fn div(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.is_empty() {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "/ requires at least one argument",
        ));
    }
    if args.len() == 1 {
        // 1 / x
        let mut acc = Acc::seed(&Num::Int(1));
        acc.apply(&as_num(&args[0], "/")?, Op::Div, false)?;
        return Ok(acc.finish());
    }
    let mut acc = Acc::seed(&as_num(&args[0], "/")?);
    for v in &args[1..] {
        acc.apply(&as_num(v, "/")?, Op::Div, false)?;
    }
    Ok(acc.finish())
}

/// `inc` / `inc'`: x + 1 with strict/primed overflow behavior. Float -> f64.
pub fn inc(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    inc_dec(args, 1, "inc", true)
}
pub fn incp(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    inc_dec(args, 1, "inc'", false)
}
pub fn dec(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    inc_dec(args, -1, "dec", true)
}
pub fn decp(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    inc_dec(args, -1, "dec'", false)
}

fn inc_dec(args: &[Value], delta: i64, name: &str, strict: bool) -> Result<Value, Throw> {
    let [v] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            &format!("{name} requires exactly 1 argument"),
        ));
    };
    let mut acc = Acc::seed(&as_num(v, name)?);
    acc.apply(&Num::Int(delta), Op::Add, strict)?;
    Ok(acc.finish())
}

// ---------------------------------------------------------------------------
// = (structural) and comparison chains
// ---------------------------------------------------------------------------

/// `=`: delegates to the canonical `eq_val` (handles the whole tower incl.
/// int<->bigint numeric equality, ratio-by-value, and the sequential group).
pub fn eq(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let all = args
        .windows(2)
        .all(|w| crate::collections::hashing::eq_val(&w[0], &w[1]));
    Ok(Value::Bool(all))
}

/// Cross-tier three-way compare. Exact for int/bigint pairs; ratios and mixed
/// tiers fall to f64 (matches mino's tower_cmp double fallback). Returns None
/// for a NaN operand (unordered).
fn tower_cmp(a: &Num, b: &Num) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Num::Int(x), Num::Int(y)) => Some(x.cmp(y)),
        (Num::Big(x), Num::Big(y)) => Some(x.cmp(y)),
        (Num::Ratio(x), Num::Ratio(y)) => Some(x.cmp(y)),
        // Exact int/bigint mixes.
        (Num::Int(x), Num::Big(y)) => Some(BigInt::from(*x).cmp(y)),
        (Num::Big(x), Num::Int(y)) => Some(x.cmp(&BigInt::from(*y))),
        // Ratio vs int/bigint: promote both to ratio (exact).
        (Num::Ratio(x), Num::Int(_) | Num::Big(_)) => Some(x.cmp(&b.to_ratio())),
        (Num::Int(_) | Num::Big(_), Num::Ratio(y)) => Some(a.to_ratio().cmp(y)),
        // Any float involved: compare as f64 (partial: NaN -> None).
        _ => a.to_f64().partial_cmp(&b.to_f64()),
    }
}

fn compare_chain(
    args: &[Value],
    name: &str,
    ok: fn(std::cmp::Ordering) -> bool,
) -> Result<Value, Throw> {
    // Single-arg / empty: short-circuit true without type-checking (mino).
    if args.len() < 2 {
        return Ok(Value::Bool(true));
    }
    let mut prev = as_num(&args[0], name)?;
    for v in &args[1..] {
        let cur = as_num(v, name)?;
        match tower_cmp(&prev, &cur) {
            Some(o) if ok(o) => {}
            _ => return Ok(Value::Bool(false)),
        }
        prev = cur;
    }
    Ok(Value::Bool(true))
}

use std::cmp::Ordering::{Equal, Greater, Less};

pub fn lt(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    compare_chain(args, "<", |o| o == Less)
}
pub fn gt(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    compare_chain(args, ">", |o| o == Greater)
}
pub fn le(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    compare_chain(args, "<=", |o| o == Less || o == Equal)
}
pub fn ge(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    compare_chain(args, ">=", |o| o == Greater || o == Equal)
}

// ---------------------------------------------------------------------------
// mod / rem / quot
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Mqr {
    Quot,
    Rem,
    Mod,
}

fn mqr(args: &[Value], op: Mqr, name: &str) -> Result<Value, Throw> {
    let [xv, yv] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            &format!("{name} requires two arguments"),
        ));
    };
    let x = as_num(xv, name)?;
    let y = as_num(yv, name)?;
    let max_tier = x.tier().max(y.tier());
    if max_tier == 3 {
        return mqr_float(x.to_f64(), y.to_f64(), op, name);
    }
    if max_tier == 2 {
        return mqr_ratio(x.to_ratio(), y.to_ratio(), op);
    }
    if max_tier == 1 {
        return mqr_big(x.to_big(), y.to_big(), op, name);
    }
    // Both Int.
    let (a, b) = match (&x, &y) {
        (Num::Int(a), Num::Int(b)) => (*a, *b),
        _ => unreachable!(),
    };
    if b == 0 {
        return Err(throw_type(&format!("{name}: division by zero")));
    }
    // LLONG_MIN / -1 overflows i64 -> promote to bigint.
    if a == i64::MIN && b == -1 {
        return mqr_big(BigInt::from(a), BigInt::from(b), op, name);
    }
    match op {
        Mqr::Quot => Ok(Value::Int(a / b)),
        Mqr::Rem => Ok(Value::Int(a % b)),
        Mqr::Mod => {
            let mut r = a % b;
            if r != 0 && (r < 0) != (b < 0) {
                r += b;
            }
            Ok(Value::Int(r))
        }
    }
}

fn mqr_big(a: BigInt, b: BigInt, op: Mqr, name: &str) -> Result<Value, Throw> {
    if b.is_zero() {
        return Err(throw_type(&format!("{name}: division by zero")));
    }
    use num_integer::Integer;
    match op {
        // Rust BigInt div/rem truncate toward zero (like C), matching quot/rem.
        Mqr::Quot => Ok(big_to_value_keep(&a / &b)),
        Mqr::Rem => Ok(big_to_value_keep(&a % &b)),
        Mqr::Mod => Ok(big_to_value_keep(a.mod_floor(&b))),
    }
}

fn mqr_ratio(a: BigRational, b: BigRational, op: Mqr) -> Result<Value, Throw> {
    if b.is_zero() {
        return Err(throw_type("division by zero"));
    }
    // trunc(a / b) is the integer quotient.
    let q = (&a / &b).trunc(); // rational with denom 1
    let qi = q.numer().clone();
    match op {
        Mqr::Quot => Ok(big_to_value_keep(qi)),
        Mqr::Rem => {
            let rem = &a - BigRational::from(qi) * &b;
            Ok(ratio_keep_tier(rem))
        }
        Mqr::Mod => {
            let rem = &a - BigRational::from(qi) * &b;
            let m = if !rem.is_zero() && rem.is_negative() != b.is_negative() {
                rem + b
            } else {
                rem
            };
            Ok(ratio_keep_tier(m))
        }
    }
}

/// Like `ratio_to_value` but keeps integer results in bigint tier (ratio
/// contagion): once through the ratio tier, integer results stay bigint.
fn ratio_keep_tier(r: BigRational) -> Value {
    if r.denom().is_one() {
        big_to_value_keep(r.numer().clone())
    } else {
        Value::Ratio(Gc::new(RatioVal(r)))
    }
}

fn mqr_float(a: f64, b: f64, op: Mqr, name: &str) -> Result<Value, Throw> {
    if a.is_nan() || a.is_infinite() {
        return Err(throw_type(&format!("{name}: NaN or Infinite dividend")));
    }
    if b.is_nan() {
        return Err(throw_type(&format!("{name}: NaN divisor")));
    }
    if b == 0.0 {
        return Err(throw_type(&format!("{name}: division by zero")));
    }
    match op {
        Mqr::Quot => {
            if b.is_infinite() {
                return Ok(Value::Float(0.0));
            }
            let r = a / b;
            Ok(Value::Float(if r >= 0.0 { r.floor() } else { r.ceil() }))
        }
        Mqr::Rem | Mqr::Mod => {
            if b.is_infinite() {
                return Ok(Value::Float(f64::NAN));
            }
            let mut r = a % b; // Rust f64 % truncates toward zero, like C fmod
            if op == Mqr::Mod && r != 0.0 && (r < 0.0) != (b < 0.0) {
                r += b;
            }
            Ok(Value::Float(r))
        }
    }
}

pub fn mod_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    mqr(args, Mqr::Mod, "mod")
}
pub fn rem(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    mqr(args, Mqr::Rem, "rem")
}
pub fn quot(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    mqr(args, Mqr::Quot, "quot")
}

// ---------------------------------------------------------------------------
// Bitwise ops (int-only, i64)
// ---------------------------------------------------------------------------

/// Grab an i64 operand for a bit op. Bigints that fit i64 are accepted;
/// non-integers throw.
fn as_long_bit(v: &Value, op: &str) -> Result<i64, Throw> {
    match v {
        Value::Int(n) => Ok(*n),
        Value::BigInt(b) => {
            b.0.to_i64()
                .ok_or_else(|| throw_type(&format!("{op} expects integers")))
        }
        _ => Err(throw_type(&format!("{op} expects integers"))),
    }
}

fn two_longs(args: &[Value], op: &str) -> Result<(i64, i64), Throw> {
    let [a, b] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            &format!("{op} requires two arguments"),
        ));
    };
    Ok((as_long_bit(a, op)?, as_long_bit(b, op)?))
}

pub fn bit_and(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_longs(args, "bit-and")?;
    Ok(Value::Int(a & b))
}
pub fn bit_or(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_longs(args, "bit-or")?;
    Ok(Value::Int(a | b))
}
pub fn bit_xor(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_longs(args, "bit-xor")?;
    Ok(Value::Int(a ^ b))
}
pub fn bit_not(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [v] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "bit-not requires one argument",
        ));
    };
    Ok(Value::Int(!as_long_bit(v, "bit-not")?))
}

fn shift_ok(b: i64) -> Result<u32, Throw> {
    if (0..64).contains(&b) {
        Ok(b as u32)
    } else {
        Err(throw_classified(
            "eval/bounds",
            "MBD001",
            "shift amount must be in [0, 63]",
        ))
    }
}

pub fn bit_shift_left(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_longs(args, "bit-shift-left")?;
    let s = shift_ok(b)?;
    // Wrapping shift: (bit-shift-left 1 63) = i64::MIN.
    Ok(Value::Int(((a as u64) << s) as i64))
}
pub fn bit_shift_right(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_longs(args, "bit-shift-right")?;
    let s = shift_ok(b)?;
    // Rust `>>` on i64 is arithmetic (sign-preserving), matching Clojure.
    Ok(Value::Int(a >> s))
}
pub fn unsigned_bit_shift_right(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_longs(args, "unsigned-bit-shift-right")?;
    let s = shift_ok(b)?;
    // Logical (zero-fill) shift via unsigned.
    Ok(Value::Int(((a as u64) >> s) as i64))
}

// ---------------------------------------------------------------------------
// Coercions: int long short byte char double float bigint rationalize
//            numerator denominator parse-long parse-double
// ---------------------------------------------------------------------------

fn one_arg<'a>(args: &'a [Value], name: &str) -> Result<&'a Value, Throw> {
    match args {
        [v] => Ok(v),
        _ => Err(throw_classified(
            "eval/arity",
            "MAR001",
            &format!("{name} requires one argument"),
        )),
    }
}

/// Extract the i64 a cast operates on, range-checking floats by their double
/// value (JVM byte/short/int check the double, not the truncated int). Chars
/// yield their codepoint. Returns the extracted i64.
fn extract_int_for_cast(v: &Value, name: &str) -> Result<i64, Throw> {
    match v {
        Value::Int(n) => Ok(*n),
        Value::Char(c) => Ok(*c as i64),
        Value::Float(d) => float_to_cast_int(*d, name),
        Value::Float32(d) => float_to_cast_int(*d as f64, name),
        Value::BigInt(b) => {
            b.0.to_i64()
                .ok_or_else(|| throw_type(&format!("{name}: bigint value out of long range")))
        }
        Value::Ratio(r) => {
            let d = r.0.to_f64().unwrap_or(f64::NAN);
            float_to_cast_int(d, name)
        }
        _ => Err(throw_type(&format!("{name}: expected a number"))),
    }
}

fn float_to_cast_int(d: f64, name: &str) -> Result<i64, Throw> {
    if d.is_nan() {
        return Err(throw_type(&format!(
            "{name}: NaN cannot be coerced to integer"
        )));
    }
    if !(LONG_MIN_F64..LONG_MAX_F64).contains(&d) {
        return Err(throw_type(&format!("{name}: value out of range")));
    }
    Ok(d as i64)
}

/// Narrow cast into [lo, hi] with the JVM float-bound-check rule: floats/ratios
/// compare their DOUBLE value against the bounds before truncation.
fn narrow_cast(v: &Value, lo: i64, hi: i64, name: &str) -> Result<Value, Throw> {
    // Chars: codepoint, range-checked.
    if let Value::Char(c) = v {
        let cp = *c as i64;
        if cp < lo || cp > hi {
            return Err(throw_type(&format!("{name}: value out of range")));
        }
        return Ok(Value::Int(cp));
    }
    // Float/ratio: check the double value directly.
    let dv = match v {
        Value::Float(d) => Some(*d),
        Value::Float32(d) => Some(*d as f64),
        Value::Ratio(r) => Some(r.0.to_f64().unwrap_or(f64::NAN)),
        _ => None,
    };
    if let Some(d) = dv {
        if d.is_nan() {
            return Err(throw_type(&format!(
                "{name}: NaN cannot be coerced to integer"
            )));
        }
        if d < lo as f64 || d > hi as f64 {
            return Err(throw_type(&format!("{name}: value out of range")));
        }
        return Ok(Value::Int(d as i64));
    }
    let ll = extract_int_for_cast(v, name)?;
    if ll < lo || ll > hi {
        return Err(throw_type(&format!("{name}: value out of range")));
    }
    Ok(Value::Int(ll))
}

pub fn int_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    narrow_cast(
        one_arg(args, "int")?,
        i32::MIN as i64,
        i32::MAX as i64,
        "int",
    )
}
pub fn long_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "long")?;
    if let Value::Char(c) = v {
        return Ok(Value::Int(*c as i64));
    }
    Ok(Value::Int(extract_int_for_cast(v, "long")?))
}
pub fn short_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    narrow_cast(one_arg(args, "short")?, -32768, 32767, "short")
}
pub fn byte_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    narrow_cast(one_arg(args, "byte")?, -128, 127, "byte")
}
pub fn char_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "char")?;
    if let Value::Char(_) = v {
        return Ok(v.clone());
    }
    let ll = extract_int_for_cast(v, "char")?;
    if !(0..=0x10FFFF).contains(&ll) {
        return Err(throw_classified(
            "eval/bounds",
            "MBD001",
            &format!("char: codepoint {ll} out of range (0..0x10FFFF)"),
        ));
    }
    if (0xD800..=0xDFFF).contains(&ll) {
        return Err(throw_classified(
            "eval/bounds",
            "MBD001",
            &format!("char: codepoint {ll} is a surrogate, not a scalar value"),
        ));
    }
    Ok(Value::Char(char::from_u32(ll as u32).unwrap()))
}

/// `(float x)` -> a 32-bit float32 in the FLT range (+/-inf throws, NaN passes).
pub fn float_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "float")?;
    let d = as_num(v, "float")?.to_f64();
    if !d.is_nan() && (d > f32::MAX as f64 || d < f32::MIN as f64) {
        return Err(throw_type("float: value out of float range"));
    }
    Ok(Value::Float32(d as f32))
}

/// `(double x)` -> a 64-bit float.
pub fn double_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "double")?;
    Ok(Value::Float(as_num(v, "double")?.to_f64()))
}

/// `(bigint x)` -> a BigInt (float/ratio truncate toward zero).
pub fn bigint(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "bigint")?;
    let b = match v {
        Value::Int(n) => BigInt::from(*n),
        Value::BigInt(b) => b.0.clone(),
        Value::Ratio(r) => r.0.trunc().numer().clone(),
        Value::Float(_) | Value::Float32(_) => {
            let d = match v {
                Value::Float32(f) => *f as f64,
                Value::Float(f) => *f,
                _ => unreachable!(),
            };
            if !d.is_finite() {
                return Err(throw_type("cannot convert non-finite double to bigint"));
            }
            use num_traits::FromPrimitive;
            BigInt::from_f64(d.trunc()).ok_or_else(|| throw_type("bigint: failed to convert"))?
        }
        _ => return Err(throw_type("bigint: expected a number")),
    };
    Ok(big_to_value_keep(b))
}

/// `(numerator r)` — ratio only (Clojure). Returns the numerator as Int/BigInt.
pub fn numerator(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match one_arg(args, "numerator")? {
        Value::Ratio(r) => Ok(big_to_value_narrow(r.0.numer().clone())),
        _ => Err(throw_type("numerator: argument must be a ratio")),
    }
}
pub fn denominator(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match one_arg(args, "denominator")? {
        Value::Ratio(r) => Ok(big_to_value_narrow(r.0.denom().clone())),
        _ => Err(throw_type("denominator: argument must be a ratio")),
    }
}

/// `(rationalize x)` — exact rational for a float; identity for exact tiers.
pub fn rationalize(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "rationalize")?;
    match v {
        Value::Int(_) | Value::BigInt(_) | Value::Ratio(_) => Ok(v.clone()),
        Value::Float(_) | Value::Float32(_) => {
            let d = match v {
                Value::Float32(f) => *f as f64,
                Value::Float(f) => *f,
                _ => unreachable!(),
            };
            let r = BigRational::from_float(d)
                .ok_or_else(|| throw_type("rationalize: non-finite float"))?;
            Ok(ratio_to_value(r))
        }
        _ => Err(throw_type("rationalize: expected a number")),
    }
}

/// `(parse-long s)` -> Int, or nil if `s` is not a whole base-10 long.
/// Matches mino: nil on empty or leading-whitespace input, and the entire
/// string must be consumed (no trailing chars).
pub fn parse_long(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match one_arg(args, "parse-long")? {
        Value::Str(s) => {
            if s.is_empty() || s.starts_with(|c: char| c.is_whitespace()) {
                return Ok(Value::Nil);
            }
            Ok(s.parse::<i64>().map_or(Value::Nil, Value::Int))
        }
        _ => Err(throw_type("parse-long: argument must be a string")),
    }
}
pub fn parse_double(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match one_arg(args, "parse-double")? {
        Value::Str(s) => {
            if s.is_empty() || s.starts_with(|c: char| c.is_whitespace()) {
                return Ok(Value::Nil);
            }
            Ok(s.parse::<f64>().ok().map_or(Value::Nil, Value::Float))
        }
        _ => Err(throw_type("parse-double: argument must be a string")),
    }
}

// ---------------------------------------------------------------------------
// unchecked-* family: two's-complement wraparound, no promotion.
// ---------------------------------------------------------------------------

/// Grab an i64 for the strict unchecked family (int only).
fn unchecked_long(v: &Value, op: &str) -> Result<i64, Throw> {
    match v {
        Value::Int(n) => Ok(*n),
        _ => Err(throw_type(&format!("{op} expects an int"))),
    }
}

/// Lenient grab for the -int family & narrowing casts: ints, floats (trunc),
/// bigints (wrap to low 64 bits), ratios (via double).
fn unchecked_long_lenient(v: &Value, op: &str) -> Result<i64, Throw> {
    match v {
        Value::Int(n) => Ok(*n),
        Value::Float(d) => Ok(trunc_to_long(*d)),
        Value::Float32(d) => Ok(trunc_to_long(*d as f64)),
        Value::BigInt(b) => Ok(b.0.to_i64().unwrap_or_else(|| {
            // Wrap: take low 64 bits as two's-complement.
            let (_sign, bytes) = b.0.to_bytes_le();
            let mut buf = [0u8; 8];
            for (i, byte) in bytes.iter().take(8).enumerate() {
                buf[i] = *byte;
            }
            let mag = u64::from_le_bytes(buf);
            let v = if b.0.is_negative() {
                mag.wrapping_neg()
            } else {
                mag
            };
            v as i64
        })),
        Value::Ratio(r) => Ok(trunc_to_long(r.0.to_f64().unwrap_or(0.0))),
        _ => Err(throw_type(&format!("{op} expects numbers"))),
    }
}

fn trunc_to_long(d: f64) -> i64 {
    if d >= LONG_MAX_F64 {
        i64::MAX
    } else if d < LONG_MIN_F64 {
        i64::MIN
    } else {
        d as i64
    }
}

fn two_unchecked(args: &[Value], op: &str, lenient: bool) -> Result<(i64, i64), Throw> {
    let [a, b] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            &format!("{op} requires exactly 2 arguments"),
        ));
    };
    if lenient {
        Ok((
            unchecked_long_lenient(a, op)?,
            unchecked_long_lenient(b, op)?,
        ))
    } else {
        Ok((unchecked_long(a, op)?, unchecked_long(b, op)?))
    }
}

fn one_unchecked(args: &[Value], op: &str, lenient: bool) -> Result<i64, Throw> {
    let [v] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            &format!("{op} requires exactly 1 argument"),
        ));
    };
    if lenient {
        unchecked_long_lenient(v, op)
    } else {
        unchecked_long(v, op)
    }
}

// 64-bit wraparound arithmetic.
pub fn unchecked_add(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_unchecked(args, "unchecked-add", false)?;
    Ok(Value::Int(a.wrapping_add(b)))
}
pub fn unchecked_subtract(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_unchecked(args, "unchecked-subtract", false)?;
    Ok(Value::Int(a.wrapping_sub(b)))
}
pub fn unchecked_multiply(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_unchecked(args, "unchecked-multiply", false)?;
    Ok(Value::Int(a.wrapping_mul(b)))
}
pub fn unchecked_inc(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-inc", false)?;
    Ok(Value::Int(x.wrapping_add(1)))
}
pub fn unchecked_dec(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-dec", false)?;
    Ok(Value::Int(x.wrapping_sub(1)))
}
pub fn unchecked_negate(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-negate", false)?;
    Ok(Value::Int(0i64.wrapping_sub(x)))
}

// Narrowing casts (two's-complement truncation to the target width).
pub fn unchecked_long_cast(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    Ok(Value::Int(one_unchecked(args, "unchecked-long", true)?))
}
pub fn unchecked_int_cast(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-int", true)?;
    Ok(Value::Int(x as i32 as i64))
}
pub fn unchecked_short_cast(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-short", true)?;
    Ok(Value::Int(x as i16 as i64))
}
pub fn unchecked_byte_cast(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-byte", true)?;
    Ok(Value::Int(x as i8 as i64))
}
pub fn unchecked_char_cast(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-char", true)?;
    // char is a 16-bit unsigned code unit.
    Ok(Value::Char(char::from_u32(x as u16 as u32).unwrap()))
}
pub fn unchecked_float_cast(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "unchecked-float")?;
    let d = as_num(v, "unchecked-float")?.to_f64();
    Ok(Value::Float32(d as f32))
}
pub fn unchecked_double_cast(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "unchecked-double")?;
    Ok(Value::Float(as_num(v, "unchecked-double")?.to_f64()))
}

// -int arithmetic family: 32-bit wraparound.
pub fn unchecked_add_int(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_unchecked(args, "unchecked-add-int", true)?;
    Ok(Value::Int((a as i32).wrapping_add(b as i32) as i64))
}
pub fn unchecked_subtract_int(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_unchecked(args, "unchecked-subtract-int", true)?;
    Ok(Value::Int((a as i32).wrapping_sub(b as i32) as i64))
}
pub fn unchecked_multiply_int(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_unchecked(args, "unchecked-multiply-int", true)?;
    Ok(Value::Int((a as i32).wrapping_mul(b as i32) as i64))
}
pub fn unchecked_inc_int(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-inc-int", true)?;
    Ok(Value::Int((x as i32).wrapping_add(1) as i64))
}
pub fn unchecked_dec_int(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-dec-int", true)?;
    Ok(Value::Int((x as i32).wrapping_sub(1) as i64))
}
pub fn unchecked_negate_int(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let x = one_unchecked(args, "unchecked-negate-int", true)?;
    Ok(Value::Int((0i32).wrapping_sub(x as i32) as i64))
}
pub fn unchecked_remainder_int(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = two_unchecked(args, "unchecked-remainder-int", true)?;
    let (ai, bi) = (a as i32, b as i32);
    if bi == 0 {
        return Err(throw_classified(
            "eval/contract",
            "MCT001",
            "unchecked-remainder-int: division by zero",
        ));
    }
    // INT_MIN % -1 == 0 (JVM), UB in C.
    if ai == i32::MIN && bi == -1 {
        return Ok(Value::Int(0));
    }
    Ok(Value::Int((ai % bi) as i64))
}

// ---------------------------------------------------------------------------
// Tier predicates: bigint? ratio? rational? decimal? (int?/float? in
// collections.rs). float?/int?/NaN? already exist there; extend as needed.
// ---------------------------------------------------------------------------

pub fn bigint_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    Ok(Value::Bool(matches!(args.first(), Some(Value::BigInt(_)))))
}
pub fn ratio_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    Ok(Value::Bool(matches!(args.first(), Some(Value::Ratio(_)))))
}
pub fn rational_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    Ok(Value::Bool(matches!(
        args.first(),
        Some(Value::Int(_) | Value::BigInt(_) | Value::Ratio(_))
    )))
}
pub fn decimal_p(_it: &mut Interp, _args: &[Value]) -> Result<Value, Throw> {
    // BigDec tier not ported; nothing is a decimal.
    Ok(Value::Bool(false))
}

#[cfg(test)]
mod tests {
    use crate::eval::Interp;
    use crate::printer::print_str;

    fn ev(it: &mut Interp, src: &str) -> String {
        print_str(&it.eval_str(src).unwrap())
    }

    #[test]
    fn overflow_throws_on_strict() {
        let mut it = Interp::new();
        assert!(it.eval_str("(+ 9223372036854775807 1)").is_err());
        assert!(it.eval_str("(* 9223372036854775807 2)").is_err());
        assert!(it.eval_str("(inc 9223372036854775807)").is_err());
        assert!(it.eval_str("(dec -9223372036854775808)").is_err());
    }

    #[test]
    fn primed_promotes() {
        let mut it = Interp::new();
        assert_eq!(
            ev(&mut it, "(+' 9223372036854775807 1)"),
            "9223372036854775808N"
        );
        assert_eq!(
            ev(&mut it, "(*' 9223372036854775807 2)"),
            "18446744073709551614N"
        );
        assert_eq!(
            ev(&mut it, "(inc' 9223372036854775807)"),
            "9223372036854775808N"
        );
        assert_eq!(
            ev(&mut it, "(-' -9223372036854775808 1)"),
            "-9223372036854775809N"
        );
    }

    #[test]
    fn exact_ratio_division() {
        let mut it = Interp::new();
        assert_eq!(ev(&mut it, "(/ 7 2)"), "7/2");
        assert_eq!(ev(&mut it, "(/ 6 2)"), "3");
        assert_eq!(ev(&mut it, "(/ 1 3)"), "1/3");
        assert_eq!(ev(&mut it, "(/ 10 4)"), "5/2");
        assert_eq!(ev(&mut it, "(/ 2)"), "1/2");
    }

    #[test]
    fn ratio_reduces_and_contagion() {
        let mut it = Interp::new();
        assert_eq!(ev(&mut it, "(+ 1/2 1/2)"), "1"); // reduces to int
        assert_eq!(ev(&mut it, "(+ 1/2 1)"), "3/2"); // ratio + int -> ratio
        assert_eq!(ev(&mut it, "(* 2/3 3)"), "2"); // reduces
        assert_eq!(ev(&mut it, "(+ 1/2 0.5)"), "1.0"); // ratio + float -> float
        assert_eq!(ev(&mut it, "(+ 1N 1)"), "2N"); // bigint stays bigint
        assert_eq!(ev(&mut it, "(+ 1N 1.0)"), "2.0"); // bigint + float -> float
    }

    #[test]
    fn eq_across_tower() {
        let mut it = Interp::new();
        assert_eq!(ev(&mut it, "(= 1 1N)"), "true");
        assert_eq!(ev(&mut it, "(= 1 1.0)"), "false");
        assert_eq!(ev(&mut it, "(= 1/2 1/2)"), "true");
        assert_eq!(ev(&mut it, "(= 2 (/ 4 2))"), "true");
        assert_eq!(ev(&mut it, "(< 1 1N)"), "false");
        assert_eq!(ev(&mut it, "(< 1N 2)"), "true");
        assert_eq!(ev(&mut it, "(> 3/2 1)"), "true");
    }

    #[test]
    fn unchecked_wrap_and_bit_shift() {
        let mut it = Interp::new();
        assert_eq!(
            ev(&mut it, "(unchecked-add 9223372036854775807 1)"),
            "-9223372036854775808"
        );
        assert_eq!(ev(&mut it, "(unchecked-byte 255)"), "-1");
        assert_eq!(ev(&mut it, "(unchecked-int 2147483648)"), "-2147483648");
        assert_eq!(ev(&mut it, "(bit-shift-left 1 63)"), "-9223372036854775808");
        assert_eq!(ev(&mut it, "(bit-shift-right -1 1)"), "-1"); // sign-preserving
        assert_eq!(
            ev(&mut it, "(unsigned-bit-shift-right -1 1)"),
            "9223372036854775807"
        );
        assert!(it.eval_str("(bit-shift-left 1 64)").is_err());
    }

    #[test]
    fn coercions_and_literals() {
        let mut it = Interp::new();
        assert_eq!(ev(&mut it, "42N"), "42N");
        assert_eq!(ev(&mut it, "22/7"), "22/7");
        assert_eq!(ev(&mut it, "0xFF"), "255");
        assert_eq!(ev(&mut it, "2r1010"), "10");
        assert_eq!(ev(&mut it, "16rFF"), "255");
        assert_eq!(ev(&mut it, "010"), "8");
        assert_eq!(ev(&mut it, "(int 3.7)"), "3");
        assert_eq!(ev(&mut it, "(double 3)"), "3.0");
        assert!(it.eval_str("(byte 300)").is_err());
        assert_eq!(ev(&mut it, "(numerator 3/4)"), "3");
        assert_eq!(ev(&mut it, "(rationalize 1.5)"), "3/2");
    }
}
