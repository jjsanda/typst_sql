// Value formatters (F-14) with locale awareness (F-38).
//
// Every formatter is a function `value => content`, so anything custom is a
// plain function away — the renderer treats them all identically. The bar to
// clear is pgfplotstable/datatool (03-latex-parity §4.2).

#import "util.typ": iso-display, parse-iso

// group: thousands separator · decimal: decimal mark · currency position
#let locales = (
  en: (group: ",", decimal: ".", currency-position: "prefix", currency-space: false),
  de: (group: ".", decimal: ",", currency-position: "suffix", currency-space: true),
  cs: (group: sym.space.thin, decimal: ",", currency-position: "suffix", currency-space: true),
  fr: (group: sym.space.thin, decimal: ",", currency-position: "suffix", currency-space: true),
  ch: (group: "'", decimal: ".", currency-position: "prefix", currency-space: true),
)

#let currency-symbols = (
  USD: (symbol: "$", digits: 2),
  EUR: (symbol: "€", digits: 2),
  GBP: (symbol: "£", digits: 2),
  CZK: (symbol: "Kč", digits: 2, position: "suffix"),
  JPY: (symbol: "¥", digits: 0),
  CHF: (symbol: "CHF", digits: 2),
  PLN: (symbol: "zł", digits: 2, position: "suffix"),
  SEK: (symbol: "kr", digits: 2, position: "suffix"),
)

#let _locale(loc) = locales.at(loc, default: locales.en)

// Insert the grouping separator into an unsigned integer digit string.
#let _group-digits(s, sep) = {
  if s.len() <= 3 { return s }
  let out = ()
  let n = s.len()
  let head = calc.rem-euclid(n, 3)
  if head > 0 { out.push(s.slice(0, head)) }
  let i = head
  while i < n {
    out.push(s.slice(i, i + 3))
    i += 3
  }
  out.join(sep)
}

// Fixed-point rendering of a number: (sign, integer digits, fraction digits).
#let _fixed(v, digits) = {
  let neg = v < 0
  let a = calc.abs(v)
  if digits == auto {
    if type(v) == int {
      (neg: neg, int: str(a), frac: "")
    } else {
      let s = str(a)
      if "e" in s or "E" in s or s == "inf" or s == "nan" {
        return (neg: neg, int: s, frac: "")
      }
      let parts = s.split(".")
      (neg: neg, int: parts.at(0), frac: if parts.len() > 1 { parts.at(1) } else { "" })
    }
  } else {
    let scaled = calc.round(float(a), digits: digits)
    let s = str(scaled)
    if "e" in s or "E" in s or s == "inf" or s == "nan" {
      return (neg: neg, int: s, frac: "")
    }
    let parts = s.split(".")
    let frac = if parts.len() > 1 { parts.at(1) } else { "" }
    while frac.len() < digits { frac += "0" }
    (neg: neg, int: parts.at(0), frac: frac.slice(0, digits))
  }
}

#let _render-number(v, digits, group, sign, loc) = {
  if v == none { return "" }
  if type(v) == str { return v }
  let l = _locale(loc)
  let f = _fixed(v, digits)
  let body = if group { _group-digits(f.int, l.group) } else { f.int }
  if f.frac.len() > 0 { body += l.decimal + f.frac }
  let prefix = if f.neg { sym.minus } else if sign and (v > 0) { "+" } else { "" }
  prefix + body
}

/// Number formatter: `qr.number(digits: 2)(1234567.891)` → 1,234,567.89
#let number(digits: auto, group: true, sign: false, locale: "en") = {
  v => _render-number(v, digits, group, sign, locale)
}

/// Currency formatter (F-14): symbol placement and digits per currency/locale.
#let currency(code, digits: auto, symbol: auto, position: auto, locale: "en") = {
  let info = currency-symbols.at(code, default: (symbol: code, digits: 2))
  let sym-str = if symbol == auto { info.symbol } else { symbol }
  let d = if digits == auto { info.at("digits", default: 2) } else { digits }
  let l = _locale(locale)
  let pos = if position == auto {
    info.at("position", default: l.currency-position)
  } else { position }
  let space = if l.currency-space { sym.space.nobreak } else { "" }
  v => {
    if v == none { return "" }
    let n = _render-number(v, d, true, false, locale)
    if pos == "prefix" { sym-str + space + n } else { n + space + sym-str }
  }
}

/// Percent formatter: `qr.percent(of: 1.0)(0.324)` → 32.4 %
#let percent(digits: 1, of: 1.0, locale: "en") = {
  v => {
    if v == none { return "" }
    _render-number(v / of * 100.0, digits, false, false, locale) + sym.space.thin + "%"
  }
}

/// Date formatter: accepts datetime or the ISO/epoch forms quarry coerces.
#let date(pattern: "[year]-[month]-[day]") = {
  v => {
    if v == none { return "" }
    if type(v) == datetime {
      v.display(pattern)
    } else if type(v) == str {
      let parsed = parse-iso(v)
      if parsed != none { parsed.display(pattern) } else { v }
    } else {
      str(v)
    }
  }
}

/// Unit formatter: number + unit with a thin space: `qr.unit("kg")(12.5)`.
#let unit(u, digits: auto, group: true, locale: "en") = {
  v => {
    if v == none { return "" }
    _render-number(v, digits, group, false, locale) + sym.space.thin + u
  }
}

/// Default rendering used by sql-table when no format is given, keyed by the
/// envelope's logical column type. Honest by default: no rounding.
#let auto-format(v, col-type, locale: "en") = {
  if v == none {
    sym.dash.em
  } else if type(v) == bool {
    if v { "true" } else { "false" }
  } else if type(v) == datetime {
    iso-display(v)
  } else if type(v) == bytes {
    // A blob is data, not text; say what it is instead of mojibake (D-06).
    sym.angle.l + str(v.len()) + " bytes" + sym.angle.r
  } else if type(v) == int {
    _render-number(v, auto, true, false, locale)
  } else if type(v) == float {
    _render-number(v, auto, false, false, locale)
  } else {
    str(v)
  }
}
