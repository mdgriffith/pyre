module.exports = grammar({
  name: "pyre",

  extras: $ => [/\s/, $.comment],

  word: $ => $.identifier,

  rules: {
    source_file: $ => repeat($._declaration),

    _declaration: $ => choice(
      $.record_declaration,
      $.session_declaration,
      $.type_declaration,
      $.operation_declaration,
      $.directive,
    ),

    record_declaration: $ => seq("record", field("name", $.type_identifier), $.block),
    session_declaration: $ => seq("session", $.block),
    type_declaration: $ => seq(
      "type",
      field("name", $.type_identifier),
      optional("="),
      $.variant,
      repeat(seq("|", $.variant)),
    ),
    variant: $ => seq(field("name", $.type_identifier), optional($.variant_body)),
    variant_body: $ => seq("{", repeat($.field_declaration), "}"),

    operation_declaration: $ => seq(
      field("operation", choice("query", "insert", "update", "delete")),
      field("name", choice($.identifier, $.type_identifier)),
      optional($.parameter_list),
      $.block,
    ),
    parameter_list: $ => seq("(", optional(commaSep1($.parameter)), ")"),
    parameter: $ => seq(field("name", $.variable), ":", field("type", $.type_expression)),

    block: $ => seq("{", repeat($._statement), "}"),
    _statement: $ => choice(
      $.directive,
      $.assignment,
      $.field_declaration,
      $.selection,
      $._statement_expression,
    ),
    _statement_expression: $ => choice(
      $.binary_expression,
      $.logical_expression,
      $.exists_expression,
      $.call_expression,
      $.list,
      $.parenthesized_expression,
      $.variable,
      $.boolean,
      $.null,
      $.number,
      $.string,
      $.qualified_identifier,
    ),
    field_declaration: $ => prec.right(seq(
      field("name", $.identifier),
      optional(":"),
      field("type", $.type_expression),
      repeat($.directive),
    )),
    selection: $ => prec.right(seq(
      optional(seq(field("alias", $.identifier), ":")),
      field("name", choice($.identifier, $.wildcard)),
      optional($.argument_list),
      optional($.block),
    )),
    assignment: $ => prec.right(3, seq(
      field("name", $.identifier),
      "=",
      field("value", $.expression),
    )),

    directive: $ => prec.right(seq(
      "@",
      field("name", $.identifier),
      optional($.argument_list),
      optional($.block),
    )),
    argument_list: $ => seq("(", optional(commaSep1($.expression)), ")"),

    expression: $ => choice(
      $.binary_expression,
      $.logical_expression,
      $.exists_expression,
      $.call_expression,
      $.list,
      $.parenthesized_expression,
      $.variable,
      $.wildcard,
      $.boolean,
      $.null,
      $.number,
      $.string,
      $.qualified_identifier,
      $.identifier,
    ),
    binary_expression: $ => prec.left(2, seq(
      field("left", $.expression),
      field("operator", choice("==", "=", "!=", ">", ">=", "<", "<=", "in", "&&", "||")),
      field("right", $.expression),
    )),
    logical_expression: $ => prec.right(1, seq(
      choice("And", "Or"),
      "(",
      commaSep1($.expression),
      ")",
    )),
    exists_expression: $ => prec.right(seq("exists", $.qualified_identifier, $.block)),
    call_expression: $ => prec(3, seq($.identifier, $.argument_list)),
    parenthesized_expression: $ => seq("(", $.expression, ")"),
    list: $ => seq("[", optional(commaSep1($.expression)), "]"),

    type_expression: $ => prec.right(seq(
      choice($.qualified_type, $.type_identifier),
      optional(seq("<", commaSep1($.type_expression), ">")),
      optional("?"),
    )),
    qualified_type: $ => seq($.type_identifier, repeat1(seq(".", choice($.type_identifier, $.identifier)))),
    qualified_identifier: $ => seq(
      choice($.identifier, $.type_identifier),
      repeat1(seq(".", choice($.identifier, $.type_identifier))),
    ),
    variable: _ => token(seq("$", /[A-Za-z_][A-Za-z0-9_]*/)),
    wildcard: _ => "*",
    boolean: _ => choice("True", "False", "true", "false"),
    null: _ => "null",
    number: _ => token(choice(
      /0x[0-9a-fA-F]+/,
      /0o[0-7]+/,
      /0b[01]+/,
      /[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/,
    )),
    string: _ => seq("\"", repeat(choice(/[^"\\\n]+/, /\\./)), "\""),
    identifier: _ => /[a-z_][A-Za-z0-9_]*/,
    type_identifier: _ => /[A-Z][A-Za-z0-9_]*/,
    comment: _ => token(seq("//", /.*/)),
  },
});

function commaSep1(rule) {
  return seq(rule, repeat(seq(",", rule)), optional(","));
}
