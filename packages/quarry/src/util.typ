// Date/time arithmetic and ISO-8601 parsing in pure Typst.
//
// The engine receives the injected clock as Unix *microseconds* (an integer,
// so no float precision is ever lost — F-03) and returns dates as ISO text,
// Julian days or Unix epochs; these helpers convert between all of them and
// Typst's datetime.

// Days from civil date (Howard Hinnant's algorithm), days since 1970-01-01.
#let days-from-civil(y, m, d) = {
  let y = if m <= 2 { y - 1 } else { y }
  let era = calc.div-euclid(y, 400)
  let yoe = y - era * 400
  let mp = calc.rem-euclid(m + 9, 12)
  let doy = calc.div-euclid(153 * mp + 2, 5) + d - 1
  let doe = yoe * 365 + calc.div-euclid(yoe, 4) - calc.div-euclid(yoe, 100) + doy
  era * 146097 + doe - 719468
}

// Inverse: civil date from days since 1970-01-01.
#let civil-from-days(z) = {
  let z = z + 719468
  let era = calc.div-euclid(z, 146097)
  let doe = z - era * 146097
  let yoe = calc.div-euclid(
    doe - calc.div-euclid(doe, 1460) + calc.div-euclid(doe, 36524) - calc.div-euclid(doe, 146096),
    365,
  )
  let y = yoe + era * 400
  let doy = doe - (365 * yoe + calc.div-euclid(yoe, 4) - calc.div-euclid(yoe, 100))
  let mp = calc.div-euclid(5 * doy + 2, 153)
  let d = doy - calc.div-euclid(153 * mp + 2, 5) + 1
  let m = calc.rem-euclid(mp + 2, 12) + 1
  (year: y + int(m <= 2), month: m, day: d)
}

// datetime → Unix microseconds (UTC; Typst datetimes are naive and quarry
// documents treat them as UTC — documented).
#let datetime-to-unix-us(dt) = {
  let days = days-from-civil(dt.year(), dt.month(), dt.day())
  let h = if dt.hour() == none { 0 } else { dt.hour() }
  let mi = if dt.minute() == none { 0 } else { dt.minute() }
  let s = if dt.second() == none { 0 } else { dt.second() }
  ((days * 24 + h) * 60 + mi) * 60 * 1000000 + s * 1000000
}

// Unix seconds → datetime.
#let unix-to-datetime(secs) = {
  let days = calc.div-euclid(secs, 86400)
  let rem = calc.rem-euclid(secs, 86400)
  let c = civil-from-days(days)
  datetime(
    year: c.year,
    month: c.month,
    day: c.day,
    hour: calc.div-euclid(rem, 3600),
    minute: calc.div-euclid(calc.rem-euclid(rem, 3600), 60),
    second: calc.rem-euclid(rem, 60),
  )
}

// Julian day (float) → datetime, second precision.
#let julian-to-datetime(jd) = {
  let unix-secs = calc.round((jd - 2440587.5) * 86400)
  unix-to-datetime(int(unix-secs))
}

#let _is-digits(s) = s.len() > 0 and s.clusters().all(c => c in "0123456789")

// Parse "YYYY-MM-DD", optionally followed by "T" or " " and "HH:MM[:SS[.fff]]",
// optionally suffixed with "Z". Returns a datetime or none.
#let parse-iso(s) = {
  let s = s.trim()
  if s.ends-with("Z") { s = s.slice(0, s.len() - 1) }
  let date-part = s
  let time-part = none
  for sep in ("T", " ") {
    if s.contains(sep) {
      let idx = s.position(sep)
      date-part = s.slice(0, idx)
      time-part = s.slice(idx + 1)
      break
    }
  }
  let dp = date-part.split("-")
  if dp.len() != 3 { return none }
  if not (_is-digits(dp.at(0)) and _is-digits(dp.at(1)) and _is-digits(dp.at(2))) {
    return none
  }
  let (y, m, d) = (int(dp.at(0)), int(dp.at(1)), int(dp.at(2)))
  if m < 1 or m > 12 or d < 1 or d > 31 { return none }
  if time-part == none {
    return datetime(year: y, month: m, day: d)
  }
  let tp = time-part.split(":")
  if tp.len() < 2 { return none }
  let sec-str = if tp.len() >= 3 { tp.at(2).split(".").at(0) } else { "0" }
  if not (_is-digits(tp.at(0)) and _is-digits(tp.at(1)) and _is-digits(sec-str)) {
    return none
  }
  let (hh, mm, ss) = (int(tp.at(0)), int(tp.at(1)), int(sec-str))
  if hh > 23 or mm > 59 or ss > 60 { return none }
  datetime(year: y, month: m, day: d, hour: hh, minute: mm, second: calc.min(ss, 59))
}

// Parse "HH:MM[:SS]" into a time-only datetime, or none.
#let parse-time(s) = {
  let tp = s.trim().split(":")
  if tp.len() < 2 { return none }
  let sec-str = if tp.len() >= 3 { tp.at(2).split(".").at(0) } else { "0" }
  if not (_is-digits(tp.at(0)) and _is-digits(tp.at(1)) and _is-digits(sec-str)) {
    return none
  }
  let (hh, mm, ss) = (int(tp.at(0)), int(tp.at(1)), int(sec-str))
  if hh > 23 or mm > 59 or ss > 60 { return none }
  datetime(hour: hh, minute: mm, second: calc.min(ss, 59))
}

#let _pad2(n) = if n < 10 { "0" + str(n) } else { str(n) }

// Render a datetime as the ISO form the engine binds/compares with.
#let iso-display(dt) = {
  let date = str(dt.year()) + "-" + _pad2(dt.month()) + "-" + _pad2(dt.day())
  if dt.hour() == none { return date }
  date + " " + _pad2(dt.hour()) + ":" + _pad2(dt.minute()) + ":" + _pad2(dt.second())
}
