import Check.Strings
import Check.Intervals

def main (args : List String) : IO UInt32 := do
  let stringsOk ← Check.Strings.run (args.headD "extract/dump.tsv")
  let errs ← Check.Intervals.failures
  for e in errs.take 20 do IO.println s!"FAIL  {e}"
  if errs.isEmpty then IO.println "OK    interval algebra"
  return if stringsOk && errs.isEmpty then 0 else 1
