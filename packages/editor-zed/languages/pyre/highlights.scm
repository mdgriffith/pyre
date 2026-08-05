(comment) @comment
(string) @string
(number) @number
(boolean) @boolean
(null) @constant.builtin
(variable) @variable.parameter
(wildcard) @variable.special
(directive_name) @attribute @constant.builtin
(record_declaration name: (type_identifier) @type)
(type_declaration name: (type_identifier) @type)
(session_declaration "session" @keyword)
(operation_declaration name: (identifier) @function)
(variant name: (type_identifier) @variant)
(type_identifier) @type
(field_declaration name: (identifier) @property)
(selection name: (identifier) @property)
(assignment name: (identifier) @property)

[
  "record"
  "type"
  "query"
  "insert"
  "update"
  "delete"
] @keyword

[
  "And"
  "Or"
  "exists"
] @keyword

[
  "=="
  "!="
  ">"
  ">="
  "<"
  "<="
  "="
  "in"
  "&&"
  "||"
  "|"
] @operator

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
  "<"
  ">"
] @punctuation.bracket

[
  ","
  "."
  ":"
] @punctuation.delimiter
