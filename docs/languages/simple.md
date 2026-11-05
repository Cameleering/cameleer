# Simple language

> Simple is Camel's built-in expression language: text templates with `${...}` functions that read the body, headers, properties and variables, plus predicates with operators such as `==`, `contains`, `regex` and `in`.

**Name:** `simple` · **Also:** `csimple` (same implementation, see [csimple](csimple.md)) · **Crate:** `cameleer-core` ·
**Apache Camel:** [Simple](https://camel.apache.org/components/next/languages/simple-language.html)

Simple is what you use most in Cameleer routes: to build a message body or a prompt, set a
header, name a file, filter messages and route them. A plain string in a YAML expression
position is Simple, so `setBody: {expression: "Hello ${body}"}` and `setBody: {simple: "Hello ${body}"}`
are the same.

Every expression and result in the tables on this page is checked by the test suite
(`crates/cameleer-main/tests/docs_languages_formats.rs`) against the message shown above the table.

## Contents

- [When to use it](#when-to-use-it)
- [Syntax in one minute](#syntax-in-one-minute)
- [Examples: routes](#examples-routes)
- [Message functions](#message-functions): body, headers, properties, variables
- [Navigating JSON and objects (OGNL)](#navigating-json-and-objects-ognl)
- [Method calls on values](#method-calls-on-values)
- [String functions](#string-functions)
- [Collection functions](#collection-functions)
- [Math functions](#math-functions)
- [Conditionals: ternary, Elvis, iif, not](#conditionals-ternary-elvis-iif-not)
- [JSON, XML and other languages inside Simple](#json-xml-and-other-languages-inside-simple)
- [Exchange, context and environment functions](#exchange-context-and-environment-functions)
- [Dates](#dates)
- [Random values and ids](#random-values-and-ids)
- [File language](#file-language)
- [Predicates and operators](#predicates-and-operators)
- [Escaping and literal text](#escaping-and-literal-text)
- [Differences from Apache Camel](#differences-from-apache-camel)
- [Common questions](#common-questions)

## When to use it

- Build a body from parts of the message: `Hello ${header.name}, your order ${body.id} shipped`.
- Build an LLM prompt from a JSON request and pick fields out of the model's JSON answer.
- Route and filter: `${header.priority} == 'high' && ${body.amount} > 1000`.
- Name files and keys: `orders/${date:now:yyyy/MM/dd}/${file:onlyname.noext}.json`.
- Small transformations without code: trim, replace, split, join, sum, hash, base64.

For heavier JSON work use [JSONPath](jsonpath.md) or [jq](jq.md) (both callable from Simple), for
XML use [XPath](xpath.md), and for real logic a script language such as [JavaScript](js.md).

## Syntax in one minute

| you write | meaning |
|---|---|
| `Hello ${body}` | a template: literal text plus functions |
| `${header.name}` | a function inside `${...}` |
| `$simple{header.name}` | the same, for places where `${` is taken (Spring-style config) |
| `${body.order.id}` | navigation into a JSON object or map (OGNL) |
| `${body.toUpperCase()}` | a method call on the value |
| `${substring(1,3)}` | a function call, applied to the body when no value is given |
| `${header.a} == 'x' && ${body} contains 'y'` | a predicate (used by `filter`, `choice`, `validate`...) |
| `${header.n > 5 ? 'big' : 'small'}` | a ternary inside one function |

A template that is one function only (`${body.order.id}`) keeps the type of the value: a number
stays a number, a list stays a list. A template with any literal text around it gives a string.

## Examples: routes

### Build a prompt for a model from a JSON request

The body is parsed with the [JSON data format](../dataformats/json.md), so Simple can navigate
into it. The prompt goes to the model (here it is just returned).

<!-- example: simple-build-prompt -->
```yaml
- route:
    id: build-prompt
    from:
      uri: direct:prompt
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: tone
            simple: "${body.tone ?: 'friendly'}"
        - setBody:
            simple: |-
              You are a support agent for ${body.product}. Answer in a ${header.tone} tone.
              Customer ${body.customer.name} asks: ${body.question.trim()}
              Known order ids: ${join(', ', '#', ${body.orders})}
```

<!-- input: simple-build-prompt -->
```json
{"product":"Acme Router","customer":{"name":"Ada"},"question":"  Why is the light red?  ","orders":[1001,1002]}
```

<!-- output: simple-build-prompt -->
```text
You are a support agent for Acme Router. Answer in a friendly tone.
Customer Ada asks: Why is the light red?
Known order ids: #1001, #1002
```

### Route on the model's answer

A model was asked to answer with JSON such as `{"category":"billing","confidence":0.93}`.
Low-confidence answers go to a human queue.

<!-- example: simple-route-on-answer -->
```yaml
- route:
    id: route-on-answer
    from:
      uri: direct:answer
      steps:
        - unmarshal:
            json: {}
        - choice:
            when:
              - simple: "${body.confidence} < 0.6"
                steps:
                  - setBody:
                      simple: "human review: ${body.category}"
              - simple: "${body.category} in 'billing,refund'"
                steps:
                  - setBody:
                      simple: "billing queue (${body.confidence})"
            otherwise:
              steps:
                - setBody:
                    simple: "general queue: ${body.category.toUpperCase()}"
```

<!-- input: simple-route-on-answer -->
```json
{"category":"billing","confidence":0.93}
```

<!-- output: simple-route-on-answer -->
```text
billing queue (0.93)
```

<!-- input: simple-route-on-answer#2 -->
```json
{"category":"refund","confidence":0.41}
```

<!-- output: simple-route-on-answer#2 -->
```text
human review: refund
```

<!-- input: simple-route-on-answer#3 -->
```json
{"category":"shipping","confidence":0.88}
```

<!-- output: simple-route-on-answer#3 -->
```text
general queue: SHIPPING
```

### Filter with headers and a typed header

<!-- example: simple-filter -->
```yaml
- route:
    id: simple-filter
    from:
      uri: direct:filter
      steps:
        - setBody:
            constant: dropped
        - filter:
            simple: "${header.priority} =~ 'HIGH' && ${headerAs(amount, Integer)} range '100..10000'"
            steps:
              - setBody:
                  simple: "kept: ${header.priority} ${header.amount}"
```

<!-- input: simple-filter {"headers":{"priority":"high","amount":"250"}} -->
```text
x
```

<!-- output: simple-filter -->
```text
kept: high 250
```

<!-- input: simple-filter#2 {"headers":{"priority":"low","amount":"250"}} -->
```text
x
```

<!-- output: simple-filter#2 -->
```text
dropped
```

### Name a file with the file language and a date

The `CamelFileName` header is what the [file](../components/file.md) consumer sets; here it is
set by hand so the example runs without a directory.

<!-- example: simple-file-name -->
```yaml
- route:
    id: simple-file-name
    from:
      uri: direct:file-name
      steps:
        - setHeader:
            name: CamelFileName
            constant: inbox/2024-06-orders.csv
        - setHeader:
            name: CamelFileLastModified
            constant:
              expression: "1718000000000"
              resultType: long
        - setBody:
            simple: "archive/${date:file:yyyy/MM}/${file:onlyname.noext}.json (${file:ext}, from ${file:parent})"
```

<!-- input: simple-file-name -->
```text
a,b
```

<!-- output: simple-file-name -->
```text
archive/2024/06/2024-06-orders.json (csv, from inbox)
```

### Increment a counter header and read the route id

<!-- example: simple-counter -->
```yaml
- route:
    id: counter-route
    from:
      uri: direct:counter
      steps:
        - setHeader:
            name: attempt
            simple: "${header.attempt}++"
        - setBody:
            simple: "route ${routeId}, attempt ${header.attempt}, body type ${bodyType}"
```

<!-- input: simple-counter {"headers":{"attempt":2}} -->
```text
x
```

<!-- output: simple-counter -->
```text
route counter-route, attempt 3, body type java.lang.String
```

### Use the exception message in an error handler

<!-- example: simple-exception -->
```yaml
- route:
    id: simple-exception
    from:
      uri: direct:exception
      steps:
        - doTry:
            steps:
              - setBody:
                  simple: "${mandatoryBodyAs(Integer)}"
            doCatch:
              - exception:
                  - java.lang.Exception
                steps:
                  - setBody:
                      simple: "failed (${exception.class}): not a number: ${body}"
```

<!-- input: simple-exception -->
```text
abc
```

<!-- output: simple-exception -->
```text
failed (NoTypeConversionAvailableException): not a number: abc
```

## Message functions

All tables in this section use this message (`body` is a string, `headers`, exchange
`properties` and `variables` are maps):

<!-- fixture: simple-msg -->
```json
{
  "body": "Hello World",
  "headers": {"name": "Ada", "amount": "250", "tags": "a,b,c", "n": 5, "priority": "high", "my-header": "dash"},
  "properties": {"region": "eu"},
  "variables": {"user": "ada"}
}
```

<!-- eval: simple-message fixture=simple-msg -->
| expression | result | notes |
|---|---|---|
| `${body}` | `Hello World` | also `${in.body}`, `${message.body}` |
| `${bodyOneLine}` | `Hello World` | the body with line breaks removed |
| `${bodyType}` | `java.lang.String` | Java-style type name of the body |
| `${bodyAs(String)}` | `Hello World` | convert the body; `mandatoryBodyAs` fails when it cannot |
| `${header.name}` | `Ada` | also `${headers.name}`, `${in.header.name}` |
| `${header[name]}` | `Ada` | bracket form |
| `${header:name}` | `Ada` | colon form |
| `${header.my-header}` | `dash` | names may contain `-` |
| `${header.missing}` | (empty) | a missing header is null, rendered as empty text |
| `${headerAs(amount,Integer)}` | `250` | header converted to a type |
| `${headers}` | `{"amount":"250","my-header":"dash","n":5,"name":"Ada","priority":"high","tags":"a,b,c"}` | all headers as a map |
| `${exchangeProperty.region}` | `eu` | also `${exchangeProperty[region]}`, `${property.region}` |
| `${exchangePropertyAs(region,String)}` | `eu` | |
| `${variable.user}` | `ada` | also `${variables.user}`, `${variable:user}` |
| `${variableAs(user,String)}` | `ada` | |
| `${variables}` | `{"user":"ada"}` | all variables as a map |
| `Dear ${header.name}, you owe ${header.amount}.` | `Dear Ada, you owe 250.` | a template |

## Navigating JSON and objects (OGNL)

When the body (or a header) is a JSON object, a map or a list, `.` and `[...]` navigate into
it. A JSON text body is parsed on the fly for navigation, so `${body.a.b}` works on the string
`{"a":{"b":7}}` too. `?.` stops at null instead of failing.

<!-- fixture: simple-order -->
```json
{
  "body": {
    "order": {"id": 42, "items": [{"sku": "A1", "qty": 2, "price": 9.5}, {"sku": "B2", "qty": 1, "price": 20}]},
    "customer": {"name": "Ada", "email": null}
  }
}
```

<!-- eval: simple-ognl fixture=simple-order -->
| expression | result | notes |
|---|---|---|
| `${body.order.id}` | `42` | keeps the number type |
| `${body.order.items[0].sku}` | `A1` | list index |
| `${body.order.items[last].price}` | `20` | `last` is the last element |
| `${body[customer][name]}` | `Ada` | bracket keys |
| `${body.customer?.email}` | (empty) | null-safe |
| `${body.missing?.email}` | (empty) | null-safe on a missing key |
| `${body.order.items.size()}` | `2` | methods work after navigation |
| `${body.customer.keySet()}` | `["name","email"]` | |
| `${body.customer.containsKey('name')}` | `true` | |
| `Order ${body.order.id} for ${body.customer.name}` | `Order 42 for Ada` | |

<!-- fixture: simple-json-text -->
```json
{"bodyText": "{\"answer\":{\"label\":\"spam\",\"score\":0.97}}"}
```

<!-- eval: simple-json-text fixture=simple-json-text -->
| expression | result |
|---|---|
| `${body.answer.label}` | `spam` |
| `${body.answer.score}` | `0.97` |

## Method calls on values

Values support the common Java `String`, `List` and `Map` methods.

<!-- eval: simple-methods fixture=simple-msg -->
| expression | result |
|---|---|
| `${body.length()}` | `11` |
| `${body.toUpperCase()}` | `HELLO WORLD` |
| `${body.toLowerCase()}` | `hello world` |
| `${body.substring(6)}` | `World` |
| `${body.substring(0,5)}` | `Hello` |
| `${body.replace('World','Camel')}` | `Hello Camel` |
| `${body.replaceAll('[aeiou]','*')}` | `H*ll* W*rld` |
| `${body.replaceFirst('l','L')}` | `HeLlo World` |
| `${body.split(' ')}` | `["Hello","World"]` |
| `${body.startsWith('Hello')}` | `true` |
| `${body.endsWith('World')}` | `true` |
| `${body.contains('lo W')}` | `true` |
| `${body.indexOf('o')}` | `4` |
| `${body.charAt(4)}` | `o` |
| `${body.matches('H.*d')}` | `true` |
| `${body.equals('Hello World')}` | `true` |
| `${body.equalsIgnoreCase('hello world')}` | `true` |
| `${body.isEmpty()}` | `false` |
| `${header.name.trim()}` | `Ada` |
| `${header.n.getClass()}` | `java.lang.Integer` |
| `${header.amount.intValue()}` | `250` |
| `${header.tags.split(',')[1]}` | `b` |

## String functions

Most functions work on the body when no value is passed, or on the last argument (which can be
any expression, such as `${header.name}`). Arguments are quoted with `'` or `"`; inside function
arguments `&comma;`, `&space;`, `&empty;`, `&quot;` and `&apos;` stand for those characters.

<!-- eval: simple-strings fixture=simple-msg -->
| expression | result | notes |
|---|---|---|
| `${uppercase()}` | `HELLO WORLD` | |
| `${lowercase(${header.name})}` | `ada` | on another value |
| `${trim('  padded  ')}` | `padded` | |
| `${normalizeWhitespace('a   b  c')}` | `a b c` | collapse runs of whitespace |
| `${substring(6)}` | `World` | drop the first n characters |
| `${substring(-6)}` | `Hello` | drop the last n characters |
| `${substring(1,1)}` | `ello Worl` | drop n from the head and m from the tail |
| `${substringBefore(' ')}` | `Hello` | |
| `${substringAfter(' ')}` | `World` | |
| `${substringBetween('H','o')}` | `ell` | |
| `${replace('World','Camel')}` | `Hello Camel` | plain text replace (all) |
| `${replace(&space;,_)}` | `Hello_World` | entity for a space |
| `${concat('a','b')}` | `ab` | |
| `${concat(${header.name},'x','-')}` | `Ada-x` | third argument is a separator |
| `${length()}` | `11` | characters (bytes for binary bodies) |
| `${split(${header.tags},',')}` | `["a","b","c"]` | string to list |
| `${hash()}` | `a591a6d40bf420404a011733cfb7b190d62c65bf0bcda32b57b277d9ad9f146e` | SHA-256 hex by default |
| `${hash('abc','SHA-1')}` | `a9993e364706816aba3e25717850c26c9cd0d89d` | |
| `${hash('abc','MD5')}` | `900150983cd24fb0d6963f7d28e17f72` | |
| `${base64Encode()}` | `SGVsbG8gV29ybGQ=` | |
| `${base64Decode('SGVsbG8=')}` | `Hello` | |
| `${isEmpty()}` | `false` | true for null, blank text, empty list or map |
| `${isEmpty('   ')}` | `true` | |
| `${isAlpha('abc')}` | `true` | |
| `${isNumeric(${header.amount})}` | `true` | digits only |
| `${isAlphaNumeric('a1')}` | `true` | |
| `${empty(string)}` | (empty) | an empty string, list (`list`) or map (`map`) |
| `${empty(list)}` | `[]` | |
| `${empty(map)}` | `{}` | |

## Collection functions

<!-- fixture: simple-list -->
```json
{"body": [3, 1, 4, 1, 5]}
```

<!-- eval: simple-collections fixture=simple-list -->
| expression | result | notes |
|---|---|---|
| `${body[1]}` | `1` | |
| `${size()}` | `5` | elements of a list or map (1 for a single value, 0 for null) |
| `${join(',')}` | `3,1,4,1,5` | |
| `${join(', ','id-')}` | `id-3, id-1, id-4, id-1, id-5` | separator and prefix |
| `${collate(2)}` | `[[3,1],[4,1],[5]]` | groups of n, handy with `split` |
| `${skip(3)}` | `[1,5]` | |
| `${sum()}` | `14` | |
| `${max()}` | `5` | |
| `${min()}` | `1` | |
| `${average()}` | `2.8` | |
| `${list('a','b','c')}` | `["a","b","c"]` | build a list |
| `${map('model','gpt-4o-mini','temperature',0)}` | `{"model":"gpt-4o-mini","temperature":"0"}` | build a map (values are text unless they are expressions) |
| `${bodyType}` | `java.util.ArrayList` | |

## Math functions

<!-- eval: simple-math fixture=simple-msg -->
| expression | result |
|---|---|
| `${abs(-5)}` | `5` |
| `${floor(2.7)}` | `2` |
| `${ceil(2.1)}` | `3` |
| `${round(2.5)}` | `3` |
| `${sum(1,2,3)}` | `6` |
| `${sum(${header.n},${header.amount})}` | `255` |
| `${max(1,7,3)}` | `7` |
| `${min(4,2,9)}` | `2` |
| `${average(1,2,3,4)}` | `2.5` |
| `${increment(${header.n})}` | `6` |
| `${decrement(${header.n})}` | `4` |
| `${header.n}++` | `6` |
| `${header.n}--` | `4` |

`${header.n}++` and `--` return the changed number; to store it, use them in a `setHeader`
(see [the counter example](#increment-a-counter-header-and-read-the-route-id)).

## Conditionals: ternary, Elvis, iif, not

<!-- eval: simple-conditionals fixture=simple-msg -->
| expression | result | notes |
|---|---|---|
| `${header.amount > 100 ? 'big' : 'small'}` | `big` | ternary: spaces around `?` and `:` are required |
| `${header.missing ?: 'default'}` | `default` | Elvis: the right side when the left is null or empty |
| `${header.name ?: 'default'}` | `Ada` | |
| `${iif(${header.amount} > 100,'big','small')}` | `big` | function form of the ternary |
| `${iif(${header.priority} == 'low',${header.name},'n/a')}` | `n/a` | |
| `${not(${header.amount} > 100)}` | `false` | negate a predicate |

## JSON, XML and other languages inside Simple

`jsonpath(...)`, `jq(...)` and `xpath(...)` run those languages on the body, or on another value
given as the first argument.

<!-- eval: simple-embedded fixture=simple-order -->
| expression | result |
|---|---|
| `${jsonpath('$.order.items[*].sku')}` | `["A1","B2"]` |
| `${jsonpath('$.order.items[?(@.qty > 1)].sku')}` | `["A1"]` |
| `${jq('.order.items \| map(.qty) \| add')}` | `3` |
| `${jq('[.order.items[] \| .qty * .price] \| add')}` | `39` |
| `${jsonpath(${body.customer},'$.name')}` | `Ada` |

`${prettyBody}` and `${pretty(value)}` print JSON indented on several lines;
`${pretty(${body.customer})}` gives:

```json
{
  "name": "Ada",
  "email": null
}
```

<!-- fixture: simple-xml -->
```json
{"body": "<order id=\"7\"><item>pen</item><item>ink</item></order>"}
```

<!-- eval: simple-xpath fixture=simple-xml -->
| expression | result |
|---|---|
| `${xpath('/order/@id')}` | `7` |
| `${xpath('count(/order/item)')}` | `2.0` |

## Exchange, context and environment functions

<!-- eval: simple-context fixture=simple-msg -->
| expression | result | notes |
|---|---|---|
| `${properties:app.greeting}` | `Hi` | a property from application.properties or `--property`; fails if missing |
| `${properties:app.missing:fallback}` | `fallback` | with a default |
| `${properties-exist:app.greeting}` | `true` | |
| `${properties-exist:app.missing}` | `false` | |
| `${env.CAMELEER_DOCS_UNSET}` | (empty) | an environment variable (also `${env:NAME}`, `${sysenv.NAME}`) |
| `${null}` | (empty) | null |
| `${type:org.apache.camel.Exchange.FILE_NAME}` | `CamelFileName` | Exchange constants (also `Integer.MAX_VALUE`, `Boolean.TRUE`...) |
| `${exception}` | (empty) | the caught exception, null when there is none |

More functions of this group, whose values change per run:

| function | value |
|---|---|
| `${exchangeId}` | the exchange id |
| `${id}`, `${messageId}` | the message id |
| `${messageTimestamp}` | the message timestamp (ms) when the consumer set one |
| `${routeId}` | id of the current route (see [the counter example](#increment-a-counter-header-and-read-the-route-id)) |
| `${fromRouteId}` | id of the route the exchange was created in |
| `${exception.message}`, `${exception.class}`, `${exception.stacktrace}` | the caught exception (see [the error handler example](#use-the-exception-message-in-an-error-handler)); `${exception.kind}` is the Cameleer error kind |
| `${sys.os.name}`, `${sys.user.dir}`, `${sys.java.io.tmpdir}`... | system properties (Java names mapped to the OS) |
| `${messageHistory}`, `${messageHistory(false)}` | the route steps the message passed (with and without exchange details) |
| `${ref:name}` | a bean or value from the registry |
| `${bean:name.method}`, `${bean:name?method=m}`, `${bean:name::m}` | call a registered bean method (see [bean](bean.md)) |

## Dates

`${date:<command>:<pattern>}` formats a date with a Java `SimpleDateFormat` pattern (`yyyy`, `MM`,
`dd`, `HH`, `mm`, `ss`, `SSS`, `EEE`, `MMM`, `Z`, `'T'`...). Commands:

- `now`, `now-1h`, `now+2d` (offsets in `ms`, `s`, `m`, `h`, `d`)
- `millis` (the current time in ms, without a pattern)
- `header.<name>`, `exchangeProperty.<name>`, `variable.<name>`: a date, a timestamp in ms, or
  an ISO-8601 text in that header, property or variable, with optional offsets
- `exchangeCreated`, `file` (the `CamelFileLastModified` header)

`${date-with-timezone:<command>:<zone>:<pattern>}` formats in a time zone (`UTC`, `Europe/Paris`,
`GMT+02:00`...). Without it dates format in the process default zone (UTC unless `TZ` is set).

<!-- fixture: simple-dates -->
```json
{"headers": {"CamelFileName": "in/orders/2024-06.csv.gz", "CamelFileLastModified": 1718000000000, "due": "2024-06-10T06:13:20Z"}}
```

<!-- eval: simple-dates fixture=simple-dates -->
| expression | result |
|---|---|
| `${date:header.CamelFileLastModified:yyyy-MM-dd'T'HH:mm}` | `2024-06-10T06:13` |
| `${date:header.CamelFileLastModified+1d:yyyy-MM-dd}` | `2024-06-11` |
| `${date:header.CamelFileLastModified-90m:HH:mm}` | `04:43` |
| `${date:file:yyyy/MM}` | `2024/06` |
| `${date:header.due:dd MMM yyyy}` | `10 Jun 2024` |
| `${date-with-timezone:header.CamelFileLastModified:Asia/Kolkata:HH:mm}` | `11:43` |
| `${date-with-timezone:header.CamelFileLastModified:GMT+02:00:HH:mm}` | `08:13` |

The current time, checked by pattern:

<!-- eval: simple-dates-now fixture=simple-dates predicate -->
| predicate | result |
|---|---|
| `${date:now:yyyy-MM-dd} regex '\d{4}-\d{2}-\d{2}'` | `true` |
| `${date:millis} > 1700000000000` | `true` |
| `${date:now-1h:HH} regex '\d{2}'` | `true` |

## Random values and ids

<!-- eval: simple-random fixture=simple-msg predicate -->
| predicate | result | notes |
|---|---|---|
| `${random(10)} range '0..9'` | `true` | 0 to max-1 |
| `${random(5,8)} range '5..7'` | `true` | min to max-1 |
| `${uuid} regex '[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[0-9a-f]{4}-[0-9a-f]{12}'` | `true` | random UUID v4 |
| `${uuid(short)} regex '[0-9a-f]{16}'` | `true` | 16 hex digits |
| `${uuid(classic)} startsWith 'ID-'` | `true` | Camel-style id |
| `${camelId} startsWith 'camel-'` | `true` | the context name (`camel-1`...), also `${camelContext.name}` |

## File language

`${file:...}` functions read the `CamelFile*` headers set by the [file](../components/file.md),
[ftp](../components/ftp.md) and similar consumers. They are used in `fileName`, `move`,
`preMove`, `doneFileName` and similar options, and anywhere in Simple.

<!-- eval: simple-file fixture=simple-dates -->
| expression | result | meaning |
|---|---|---|
| `${file:name}` | `in/orders/2024-06.csv.gz` | file name relative to the endpoint directory |
| `${file:name.noext}` | `in/orders/2024-06` | without any extension |
| `${file:name.noext.single}` | `in/orders/2024-06.csv` | without the last extension |
| `${file:name.ext}` | `csv.gz` | all extensions (also `${file:ext}`) |
| `${file:name.ext.single}` | `gz` | the last extension |
| `${file:onlyname}` | `2024-06.csv.gz` | name without directories |
| `${file:onlyname.noext}` | `2024-06` | |
| `${file:onlyname.noext.single}` | `2024-06.csv` | |
| `${file:ext}` | `csv.gz` | |
| `${file:parent}` | `in/orders` | parent directory |
| `${file:path}` | `in/orders/2024-06.csv.gz` | the path (`CamelFilePath`) |
| `${file:absolute}` | `false` | whether the path is absolute |
| `${file:absolute.path}` | `in/orders/2024-06.csv.gz` | the absolute path (`CamelFileAbsolutePath` when set) |
| `${file:length}` | `0` | size in bytes (`CamelFileLength`, also `${file:size}`) |
| `${file:modified}` | `1718000000000` | last modified, ms |
| `${date:file:yyyyMMdd}` | `20240610` | last modified, formatted |

Typical uses:

- `move: "done/${date:now:yyyy/MM/dd}/${file:name}"`: archive by day.
- `fileName: "${file:onlyname.noext}.json"`: same name, new extension.
- `moveFailed: "error/${file:name}.${exchangeId}"`.

## Predicates and operators

A predicate is `left operator right`, combined with `&&` / `and` and `||` / `or` (`&&` binds
tighter). The left side is usually a `${...}` function; the right side a quoted literal, a number,
`null`, `true`/`false` or another function. When one side is a number (an unquoted literal
such as `100`, or a numeric header), a numeric text on the other side is compared as a number:
`${header.amount} > 100` is true for the text `250`. When both sides are text, they are compared
as text, and `range 'a..b'` bounds are always text: convert first with
`${headerAs(amount,Integer)}`. A predicate without an operator is true when the value is
`true` (or the text `true`) and false for null, `false` and empty values.

<!-- eval: simple-operators fixture=simple-msg predicate -->
| predicate | result | operator |
|---|---|---|
| `${header.name} == 'Ada'` | `true` | equals |
| `${header.name} != 'Bob'` | `true` | not equals |
| `${header.name} =~ 'ADA'` | `true` | equals, ignoring case |
| `${header.name} !=~ 'ADA'` | `false` | not equals, ignoring case |
| `${header.amount} > 100` | `true` | numeric when both sides are numbers |
| `${header.amount} >= 250` | `true` | |
| `${header.n} < 10` | `true` | |
| `${header.n} <= 4` | `false` | |
| `${body} contains 'World'` | `true` | substring, list element or map key |
| `${body} !contains 'Moon'` | `true` | also `not contains` |
| `${body} ~~ 'world'` | `true` | contains, ignoring case |
| `${body} !~~ 'WORLD'` | `false` | |
| `${body} regex 'Hello \w+'` | `true` | the whole value must match |
| `${body} !regex '\d+'` | `true` | also `not regex` |
| `${header.priority} in 'high,urgent'` | `true` | comma-separated list |
| `${header.priority} !in 'low,normal'` | `true` | also `not in` |
| `${headerAs(amount,Integer)} range '100..500'` | `true` | inclusive range (convert text to a number first, see below) |
| `${headerAs(amount,Integer)} !range '1..99'` | `true` | also `not range` |
| `${header.amount} range '1..99'` | `true` | pitfall: the text `250` compared as text |
| `${header.n} is Integer` | `true` | type check: `String`, `Integer`, `Number`, `Boolean`, `List`, `Map`... |
| `${header.name} !is Integer` | `true` | also `not is` |
| `${body} startsWith 'Hello'` | `true` | also `starts with` |
| `${body} endsWith 'World'` | `true` | also `ends with` |
| `${body} !startsWith 'World'` | `true` | |
| `${header.missing} == null` | `true` | |
| `${header.name} == ${variable.user}` | `false` | function on both sides (case matters) |
| `${header.name} =~ ${variable.user}` | `true` | |
| `${header.name} == 'Ada' && ${header.n} > 3` | `true` | and |
| `${header.name} == 'Bob' \|\| ${header.n} > 3` | `true` | or |
| `${header.name} == 'Bob' or ${header.n} > 3 and ${header.priority} == 'high'` | `true` | `and` binds tighter |
| `${header.missing}` | `false` | null is false |
| `${isEmpty(${header.missing})}` | `true` | a boolean function |
| `${body.length()} > 5` | `true` | |

## Escaping and literal text

<!-- eval: simple-escaping fixture=simple-msg -->
| expression | result | notes |
|---|---|---|
| `\${body}` | `${body}` | a literal `${` |
| `$${body}` | `${body}` | the same |
| `$simple{body}` | `Hello World` | alternative function syntax |
| `price: 10\}` | `price: 10}` | a literal `}` |

`\n`, `\t` and `\r` in a Simple text become a newline, tab and carriage return, so
`Line1\nLine2` gives two lines.

In route files, `{{name}}` is a property placeholder resolved when the route loads, before
Simple runs. Use `${properties:name}` to read a property at run time.

## Options

Simple has no language options. `resultType` given on a `simple:` expression in YAML converts
the result, as for any language.

## Rust DSL

```rust
b.from("direct:simple-rust")
    .set_header("greeting", simple("Hello ${body}"))
    .filter(simple("${header.greeting} contains 'World'"))
    .set_body(simple("${header.greeting.toUpperCase()}!"))
    .end();
```

## Differences from Apache Camel

- No Java objects: values are JSON-like (string, number, boolean, list, map, bytes), so OGNL
  calls are limited to the common `String`, `List` and `Map` methods listed above, and
  `${bodyType}` reports the Java type name that matches the value.
- `${body.a.b}` on a JSON text body navigates into the parsed JSON; Camel needs a converted body
  for that.
- `${sys.*}` maps the common Java system property names (`os.name`, `user.dir`, `user.home`,
  `java.io.tmpdir`, `line.separator`...) to the operating system.
- `${type:...}` knows the `org.apache.camel.Exchange` constants and a few `java.lang` ones only.
- `${exception.class}` is the Cameleer error kind (for example `NoTypeConversionAvailableException`),
  not a Java class name.
- `csimple` is the same interpreter, not compiled Java.

## Common questions

### How do I read a JSON field in Simple?

Unmarshal the body with the [JSON data format](../dataformats/json.md) and use `${body.field.sub}`,
or call `${jsonpath('$.field.sub')}` directly on the JSON text. A JSON text body also works with
`${body.field}` for simple navigation.

### How do I give a default when a header is missing?

Use the Elvis operator: `${header.region ?: 'eu'}`. It also replaces an empty string.

### Why does my comparison of two numbers behave like text?

Both sides are text. `${header.amount} > 100` compares numbers (the literal `100` is a number),
but `${header.amount} > '99'` and `${header.amount} range '1..99'` compare text, so `250` sorts
before `99`. Write numbers unquoted, or convert the header with `${headerAs(amount,Integer)}`.
Apache Camel converts the range bounds to the type of the left side, so this differs.

### How do I get the current date in a file name?

`${date:now:yyyyMMdd}`, for example `fileName: "report-${date:now:yyyy-MM-dd}.csv"`. Add
`date-with-timezone:now:Europe/Paris:` to format in another zone.

### How do I write `${` literally, for example in a template sent to another system?

Write `\${` or `$${`. To avoid Simple completely, use the [constant](constant.md) language.

## Related

- [csimple](csimple.md), [constant](constant.md), [header](header.md),
  [exchangeProperty](exchangeProperty.md), [variable](variable.md)
- [JSONPath](jsonpath.md), [jq](jq.md), [XPath](xpath.md): structured data
- [JSON data format](../dataformats/json.md)
- [File component](../components/file.md): where the file language is used most
- [Apache Camel Simple language](https://camel.apache.org/components/next/languages/simple-language.html)
- [Apache Camel File language](https://camel.apache.org/components/next/languages/file-language.html)
