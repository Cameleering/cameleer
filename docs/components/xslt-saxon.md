# XSLT Saxon component

> The `xslt-saxon` component transforms XML with an XSLT stylesheet like `xslt`, under the scheme Camel uses for Saxon; `version="2.0"` stylesheets get XPath 2.0 and the XSLT 2.0 core.

**Scheme:** `xslt-saxon:` · **Crate:** `cameleer-xml` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [XSLT Saxon](https://camel.apache.org/components/next/xslt-saxon-component.html)

## When to use it

- Run routes written for Camel's `xslt-saxon` component without changing them.
- Group and aggregate XML with `xsl:for-each-group`, write reusable `xsl:function`s, or split
  text with `xsl:analyze-string` (XSLT 2.0).
- Summarize a large XML export into a short report that fits in an LLM context window.

There is no Saxon inside Cameleer: `xslt-saxon` and [`xslt`](xslt.md) use the same built-in
processor. The XSLT version comes from the stylesheet's `version` attribute in both cases.

## URI format

```text
xslt-saxon:templateResource[?options]
```

## Example

Group sales lines by region and total them, an XSLT 2.0 task (`for-each-group`,
`current-group()`, `string-join`).

<!-- file: xsl/sales-by-region.xsl -->
```xml
<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:output method="text"/>
  <xsl:template match="/sales">
    <xsl:for-each-group select="sale" group-by="@region">
      <xsl:sort select="current-grouping-key()"/>
      <xsl:value-of select="current-grouping-key()"/>
      <xsl:text>: </xsl:text>
      <xsl:value-of select="sum(current-group()/@amount)"/>
      <xsl:text> (</xsl:text>
      <xsl:value-of select="string-join(current-group()/@rep, ', ')"/>
      <xsl:text>)&#10;</xsl:text>
    </xsl:for-each-group>
  </xsl:template>
</xsl:stylesheet>
```

<!-- example: xslt-saxon-group-by -->
```yaml
- route:
    id: xslt-saxon-group
    from:
      uri: direct:xslt-saxon-group
      steps:
        - to: xslt-saxon:xsl/sales-by-region.xsl
```

<!-- input: xslt-saxon-group-by -->
```xml
<sales>
  <sale region="west" rep="ann" amount="100"/>
  <sale region="east" rep="bob" amount="250"/>
  <sale region="west" rep="cid" amount="50"/>
</sales>
```

<!-- output: xslt-saxon-group-by -->
```text
east: 250 (bob)
west: 150 (ann, cid)
```

## Rust DSL

```rust
b.from("direct:xslt-saxon-rust")
    .to("xslt-saxon:xsl/sales-by-region.xsl");
```

## Options

The options of the [xslt component](xslt.md#options): `output`, `contentCache` (default
`true`), `failOnNullBody`, `deleteOutputFile`, `allowTemplateFromHeader`. Also accepted and
ignored: `saxonExtensionFunctions`, `saxonConfiguration`, `saxonReaderProperties`,
`secureProcessing`, plus the Java-only settings listed on the xslt page.

### XSLT 2.0 support

With `version="2.0"`: XPath 2.0 expressions, `xsl:for-each-group` (`group-by`,
`group-adjacent`, `group-starting-with`, `group-ending-with`), `xsl:function`,
`xsl:sequence`, typed variables and parameters (`as`), `xsl:analyze-string` with
`regex-group()`, tunnel parameters, `xsl:result-document`, `xsl:next-match`, `xsl:namespace`,
`xsl:perform-sort`, `xsl:value-of` with `separator`. XSLT 3.0-only instructions
(`xsl:iterate`, `xsl:try`, `xsl:merge`) and schema-aware processing fail with a
`TransformerConfigurationException`. Saxon extension functions are not available.

## Message headers

Same as [xslt](xslt.md#message-headers): `CamelXsltFileName`, `CamelXsltStylesheet`,
`CamelXsltResourceUri`, and every header as an `xsl:param`.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/sales.camel.yaml
```

## Common questions

### Do I need a Saxon license or jar?

No. Cameleer has its own processor; the scheme only exists so Camel routes keep working.

### What is the difference between `xslt` and `xslt-saxon` here?

Only the extra accepted Saxon option names. Both run XSLT 1.0, and XSLT 2.0 when the
stylesheet says `version="2.0"`.

### My stylesheet uses `xsl:iterate`. What can I do?

Rewrite it with recursion (a named template or an `xsl:function`) or `xsl:for-each`; XSLT 3.0
is not supported.

## Related

- [xslt](xslt.md), [xquery](xquery.md), [xj](xj.md).
- [Apache Camel XSLT Saxon component](https://camel.apache.org/components/next/xslt-saxon-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
