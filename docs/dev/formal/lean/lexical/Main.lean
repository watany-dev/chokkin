import Lexical

/-!
`oracle FILE...`: for each Python source file, one JSON line with what the
specs in `Lexical/` say about it. The harness (`harness/`) compares it with
`chokkin --reporter json` and with CPython's `tokenize`.
-/

open Lexical

def jsonStr (s : String) : String :=
  let esc := s.foldl (init := "") fun acc c =>
    if c == '"' then acc ++ "\\\""
    else if c == '\\' then acc ++ "\\\\"
    else if c.val < 0x20 || c.val == 0x7f || (c.val ≥ 0x80 && c.val < 0xa0) || c.val == 0x2028 || c.val == 0x2029 then
      let h := Nat.toDigits 16 c.val.toNat
      acc ++ "\\u" ++ String.mk (List.replicate (4 - h.length) '0' ++ h)
    else acc.push c
  "\"" ++ esc ++ "\""

def jsonList (xs : List String) : String := "[" ++ ",".intercalate xs ++ "]"

def jsonOpt (o : Option Nat) : String := match o with | some n => toString n | none => "null"

def decisionJson : Decision → String
  | .utf8 b => jsonStr (if b then "utf-8-bom" else "utf-8")
  | .codec m => jsonStr m
  | .unknown n => jsonStr ("unknown:" ++ n)
  | .bomMismatch n => jsonStr ("bom-mismatch:" ++ n)

/-- The text the tokenizer sees, for the codecs the oracle decodes itself. -/
def decodeFor (d : Decision) (bytes : ByteArray) : Option String :=
  match d with
  | .utf8 bom =>
    let body := if bom then bytes.extract 3 bytes.size else bytes
    String.fromUTF8? body
  | .codec "latin_1" => some (String.mk (bytes.toList.map (fun b => Char.ofNat b.toNat)))
  | _ => none

def report (path : String) : IO Unit := do
  let bytes ← IO.FS.readBinFile path
  let raw := bytes.toList.map (fun b => Char.ofNat b.toNat)
  let d := decide raw
  let fields := [("path", jsonStr path), ("decision", decisionJson d)]
  match decodeFor d bytes with
  | none =>
    IO.println ("{" ++ ",".intercalate (fields.map fun (k, v) => jsonStr k ++ ":" ++ v) ++ ",\"decoded\":false}")
  | some text =>
    let s := text.toList
    let lx := lex s
    let eff := effects lx.comments lx.firstCode
    let more := [
      ("decoded", "true"),
      ("lines", toString lx.lines),
      ("error", match lx.error with
        | some (l, m) => "{\"line\":" ++ toString l ++ ",\"msg\":" ++ jsonStr m ++ "}"
        | none => "null"),
      ("firstCode", jsonOpt lx.firstCode),
      ("comments", jsonList (lx.comments.map fun c =>
        "[" ++ toString c.line ++ "," ++ jsonStr (String.mk c.text) ++ "]")),
      ("inline", jsonList (eff.inline.map fun (l, cs) =>
        "[" ++ toString l ++ "," ++ jsonList (cs.map jsonStr) ++ "]")),
      ("fileCodes", jsonList (eff.fileCodes.map jsonStr)),
      ("warnings", jsonList (eff.warnings.map fun (l, w) =>
        "[" ++ toString l ++ "," ++ jsonStr w.name ++ "]")),
      ("scripts", jsonList ((scriptBlocks s).map fun b =>
        "[" ++ toString b.opening ++ "," ++ toString b.closing ++ "," ++ jsonStr b.type ++ "]"))
    ]
    IO.println ("{" ++ ",".intercalate ((fields ++ more).map fun (k, v) => jsonStr k ++ ":" ++ v) ++ "}")

def main (args : List String) : IO Unit := do
  for p in args do
    report p
