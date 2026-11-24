/-
Baseline fingerprints (`rule:target`, spec §9) join parts with `:`, so a
part must not contain a bare `:` and two different parts must never
encode the same way. The reference encoding is RFC 3986 §2.1
percent-encoding restricted to the two octets that need it here: `%`
(the escape character itself) as `%25` and `:` as `%3A`.
-/

namespace Spec.TargetPart

/-- RFC 3986 §2.1 percent-decoding of `%25` and `%3A`; any other text is
    left as is. -/
def decode : List Char → List Char
  | '%' :: '2' :: '5' :: rest => '%' :: decode rest
  | '%' :: '3' :: 'A' :: rest => ':' :: decode rest
  | c :: rest => c :: decode rest
  | [] => []

/-- What an encoder must satisfy: no `:` in its output, and decoding gives
    the input back (which makes it injective). -/
def sound (enc : String → String) (raw : String) : Bool :=
  !(enc raw).toList.contains ':' && decode (enc raw).toList == raw.toList

end Spec.TargetPart
