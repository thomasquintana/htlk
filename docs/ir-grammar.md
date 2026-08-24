# IR Grammar

Harness Toolkit IR is a declarative language for defining executable tasks and
runtime states. IR source files use the `.qir` extension.

The language is still under development. This document records the syntax used
by the current IR examples and will evolve with the parser and compiler.

## Notation

The grammar uses an EBNF-like notation:

- `"text"` denotes literal source text.
- `rule` denotes another grammar production.
- `( ... )` groups productions.
- `A | B` selects either `A` or `B`.
- `[ ... ]` denotes an optional production.
- `{ ... }` denotes zero or more repetitions.

Whitespace separates tokens but is otherwise insignificant, except inside
string literals. A line comment begins with `//` and continues to the end of
the line.

## Source File

```ebnf
source_file = { import_decl | binding_decl | task_decl | state_decl } ;

import_decl = "import", qualified_name, "as", identifier ;
binding_decl = identifier, "=", expression ;
qualified_name = identifier, { ".", identifier } ;
```

An import binds a qualified module name to a local alias:

```qir
import runtime.llms as llms
import runtime.tools as tools
```

A top-level binding associates a name with an expression, such as a reusable
prompt template:

```qir
prompt_template = """
Summarize the supplied text.
"""
```

## Declarations

Tasks define executable steps with inputs and outputs:

```ebnf
task_decl = "task", identifier, "{",
              inputs_decl,
              outputs_decl,
              steps_decl,
            "}" ;

inputs_decl = "inputs", [ "=" ], object ;
outputs_decl = "outputs", [ "=" ], ( type | object ) ;
steps_decl = "steps", block ;
```

States define lifecycle guards and actions:

```ebnf
state_decl = "state", identifier, "{",
               inputs_decl,
               outputs_decl,
               [ guards_decl ],
               [ actions_decl ],
             "}" ;

guards_decl = "guards", "{",
                [ "on_enter", lua_block ],
                [ "on_exit", lua_block ],
              "}" ;

actions_decl = "actions", "{",
                 [ "on_enter", lua_block ],
                 [ "on_exit", lua_block ],
                 [ "on_error", lua_block ],
               "}" ;

lua_block = "{", embedded_lua, "}" ;
```

Guard and action blocks contain Lua 5.4 executed by the runtime's embedded
[`mlua`](https://github.com/mlua-rs/mlua) interpreter. The runtime exposes
harness values and operations, including
`context`, `call`, and `template`, to that interpreter:

```qir
guards {
    on_enter {
        if context.inputs.prompt == nil then
            error("A prompt is required.")
        end
    }
}
```

The optional `=` in input and output declarations reflects both forms present
in current IR examples. A canonical form will be selected when the parser is
implemented.

## Statements

```ebnf
block = "{", { statement }, "}" ;

statement = assignment
          | augmented_assignment
          | expression
          | loop_statement
          | parallel_for_statement
          | if_statement
          | return_statement
          | break_statement ;

assignment = identifier, "=", expression ;
augmented_assignment = identifier, "+=", expression ;
return_statement = "return", [ expression ] ;
break_statement = "break" ;

loop_statement = "loop", block ;
if_statement = "if", expression, block ;

parallel_for_statement = "parallel", "for", identifier, "in", expression,
                         "do", block, [ "with", "limit", integer ] ;
```

For example:

```qir
parallel for prompt in results do {
    subprompts += llms.reason {
        "prompt": prompt
    }
} with limit 8
```

## Expressions

```ebnf
expression = postfix_expression, { binary_operator, postfix_expression } ;
postfix_expression = primary, { postfix } ;

primary = invocation
        | literal
        | identifier
        | list_literal
        | object ;

postfix = ".", identifier ;
binary_operator = "==" ;

invocation = qualified_name, object ;
list_literal = "[", [ expression, { ",", expression }, [ "," ] ], "]" ;
object = "{", [ member, { ",", member }, [ "," ] ], "}" ;
member = string, ":", expression ;

literal = string | integer | float | boolean | null ;
boolean = "true" | "false" ;
null = "null" ;
```

Member access uses dot notation, as in `query.semantic_query` or
`results.length`. Calls use a qualified task name followed by an object whose
members are the named arguments:

```qir
response = tools.call {
    "name": "runtime.llms.reason",
    "args": {
        "prompt": prompt,
        "temperature": 0.0
    }
}
```

The built-in `list` expression also uses invocation syntax:

```qir
results = list {
    "unique": true,
    "values": subprompts
}
```

## Types

Type expressions are strings in the current syntax:

```ebnf
type = string ;
```

Scalar type names used by the current examples are `string`, `integer`,
`float`, and `object`. Collection types use brackets, such as `list[string]`
and `list[object]`. Structured types are represented by objects and lists:

```qir
inputs = {
    "prompt": "string",
    "context": [{
        "role": "string",
        "content": "string"
    }]
}
```

## Strings

```ebnf
string = quoted_string | multiline_string ;
quoted_string = '"', { character | escape }, '"' ;
multiline_string = '"""', { character }, '"""' ;
```

Quoted strings support escapes. Triple-quoted strings preserve multiline text.
The runtime renders templates with [Tera](https://keats.github.io/tera/docs/).
Tera expressions use `{{` and `}}`; control structures use `{%` and `%}`:

````qir
"prompt": "{{ template }}\n\n```{{ prompt }}```"
````

## Lexical Tokens

```ebnf
identifier = ( letter | "_" ), { letter | digit | "_" } ;
integer = [ "-" ], digit, { digit } ;
float = [ "-" ], digit, { digit }, ".", digit, { digit } ;

letter = "A" ... "Z" | "a" ... "z" ;
digit = "0" ... "9" ;
```

Keywords are reserved and cannot be used as identifiers. The keywords shown
in the current grammar are `actions`, `as`, `break`, `do`, `false`, `for`,
`guards`, `if`, `import`, `in`, `inputs`, `limit`, `loop`, `null`, `on_enter`,
`on_error`, `on_exit`, `outputs`, `parallel`, `return`, `state`, `steps`,
`task`, `true`, and `with`.
