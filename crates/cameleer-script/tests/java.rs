//! Java (jOOR) language: table-driven tests.

mod common;
use common::*;

#[test]
fn java_bindings() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "java",
        &ex,
        &[
            ("return body;", "\"Hello World\""),
            ("\"Hi \" + body", "\"Hi Hello World\""),
            ("return message.getBody();", "\"Hello World\""),
            ("return exchange.getIn().getHeader(\"foo\");", "\"bar\""),
            ("return header(\"foo\");", "\"bar\""),
            ("return headerAs(\"num\", Integer.class) + 1;", "6"),
            ("return bodyAs(String.class).toUpperCase();", "\"HELLO WORLD\""),
            ("return optionalBody.isPresent();", "true"),
            ("return optionalBody.map(b -> b.toString().length()).orElse(0);", "11"),
            ("return exchangeProperty(\"p1\");", "\"v1\""),
            ("return variable(\"v1\");", "\"x\""),
            ("String b = (String) body; return b.substring(0, 5);", "\"Hello\""),
            ("Object o = body; if (o instanceof String s) { return s.length(); } return -1;", "11"),
            ("var h = header(\"num\"); return h;", "5"),
        ],
    );
}

#[test]
fn java_statements() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "joor",
        &ex,
        &[
            ("int s = 0; for (int i = 1; i <= 10; i++) { s += i; } return s;", "55"),
            ("int n = 0; while (n < 7) n += 3; return n;", "9"),
            ("int n = 10; do { n--; } while (n > 5); return n;", "5"),
            ("int x = 5; if (x > 3) { return \"big\"; } else { return \"small\"; }", "\"big\""),
            ("return 7 / 2;", "3"),
            ("return 7.0 / 2;", "3.5"),
            ("return 7 % 3;", "1"),
            ("int a = 3; return a > 2 ? \"y\" : \"n\";", "\"y\""),
            ("String s = \"a\"; s += 1 + 2; return s;", "\"a3\""),
            ("return \"x\" + 1 + 2;", "\"x12\""),
            ("return 1 + 2 + \"x\";", "\"3x\""),
            ("long l = 1L << 40; return l;", "1099511627776"),
            ("char c = 'a'; return c;", "\"a\""),
            ("String day = \"TUE\"; switch (day) { case \"MON\": return 1; case \"TUE\": return 2; default: return 0; }", "2"),
            ("int k = 2; String r = switch (k) { case 1 -> \"one\"; case 2, 3 -> \"two-three\"; default -> \"many\"; }; return r;", "\"two-three\""),
            ("int k = 5; int r = switch (k) { case 1: yield 10; default: yield k * 2; }; return r;", "10"),
            ("int s = 0; for (int v : new int[]{1, 2, 3}) s += v; return s;", "6"),
            ("int s = 0; outer: for (int i = 0; i < 3; i++) { for (int j = 0; j < 3; j++) { if (j == 1) continue outer; s++; } } return s;", "3"),
            ("try { throw new IllegalStateException(\"bad\"); } catch (IllegalStateException e) { return \"caught \" + e.getMessage(); }", "\"caught bad\""),
            ("try { Integer.parseInt(\"x\"); return \"ok\"; } catch (NumberFormatException e) { return \"nfe\"; } finally { }", "\"nfe\""),
            ("try { Object o = null; o.toString(); } catch (NullPointerException | IllegalArgumentException e) { return \"npe\"; } return \"none\";", "\"npe\""),
            ("final int x = 3; final String y = \"v\"; return y + x;", "\"v3\""),
            ("int[] arr = {3, 1, 2}; Arrays.sort(arr); return arr;", "[1,2,3]"),
            ("int[] arr = new int[3]; arr[1] = 5; return arr;", "[0,5,0]"),
            ("return (int) 3.9;", "3"),
            ("return (double) 3 / 2;", "1.5"),
            ("return Integer.MAX_VALUE;", "2147483647"),
        ],
    );
    check_err(
        "java",
        &ex,
        &[
            ("throw new IllegalArgumentException(\"nope\");", "nope"),
            ("return 1 / 0;", "/ by zero"),
        ],
    );
}

#[test]
fn java_library() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "java",
        &ex,
        &[
            ("List<String> l = new ArrayList<>(); l.add(\"a\"); l.add(\"b\"); return l.size() + \":\" + l.get(1);", "\"2:b\""),
            ("Map<String, Integer> m = new HashMap<>(); m.put(\"a\", 1); m.put(\"b\", 2); return m.get(\"b\") + m.getOrDefault(\"c\", 10);", "12"),
            ("Map<String, Object> m = new LinkedHashMap<>(); m.put(\"x\", 1); return m.containsKey(\"x\") && !m.isEmpty();", "true"),
            ("List<Integer> l = Arrays.asList(3, 1, 2); Collections.sort(l); return l;", "[1,2,3]"),
            ("return List.of(1, 2, 3).stream().map(x -> x * 2).collect(Collectors.toList());", "[2,4,6]"),
            ("return List.of(1, 2, 3, 4).stream().filter(x -> x % 2 == 0).mapToInt(Integer::intValue).sum();", "6"),
            ("return Stream.of(\"a\", \"b\").collect(Collectors.joining(\",\"));", "\"a,b\""),
            ("return Map.of(\"k\", \"v\").get(\"k\");", "\"v\""),
            ("return String.join(\"-\", List.of(\"a\", \"b\"));", "\"a-b\""),
            ("return String.format(\"%s=%d\", \"n\", 5);", "\"n=5\""),
            ("return \"  pad \".trim().length();", "3"),
            ("return \"a,b,c\".split(\",\").length;", "3"),
            ("return \"Hello\".charAt(1);", "\"e\""),
            ("return \"Hello\".indexOf(\"l\") + \"Hello\".lastIndexOf(\"l\");", "5"),
            ("return \"abc\".contains(\"b\") && \"abc\".startsWith(\"a\") && \"abc\".endsWith(\"c\");", "true"),
            ("return \"abc\".replace(\"b\", \"X\").toUpperCase();", "\"AXC\""),
            ("return \"ab\".equals(\"ab\") && \"AB\".equalsIgnoreCase(\"ab\");", "true"),
            ("return \"\".isEmpty() && \" \".isBlank();", "true"),
            ("return Integer.parseInt(\"12\") + Integer.valueOf(3);", "15"),
            ("return Math.max(3, 7) + Math.abs(-2) + Math.round(2.6);", "12"),
            ("return Math.sqrt(16.0);", "4.0"),
            ("return Double.parseDouble(\"1.5\") * 2;", "3.0"),
            ("return Boolean.parseBoolean(\"true\");", "true"),
            ("return String.valueOf(42).length();", "2"),
            ("StringBuilder sb = new StringBuilder(); sb.append(\"a\").append(1); return sb.toString();", "\"a1\""),
            ("List<String> l = new ArrayList<>(List.of(\"b\", \"a\")); l.sort(Comparator.naturalOrder()); return l;", "[\"a\",\"b\"]"),
            ("List<String> l = new ArrayList<>(); for (String s : \"x y z\".split(\" \")) l.add(s.toUpperCase()); return String.join(\"\", l);", "\"XYZ\""),
            ("Function<Integer, Integer> f = x -> x + 1; return f.apply(4);", "5"),
            ("BiFunction<Integer, Integer, Integer> add = (a, b) -> a + b; return add.apply(2, 3);", "5"),
            ("Supplier<String> s = () -> \"sup\"; return s.get();", "\"sup\""),
            ("Predicate<String> p = s -> s.isEmpty(); return p.test(\"\");", "true"),
            ("Map<String, Integer> m = new LinkedHashMap<>(); m.put(\"a\", 1); m.put(\"b\", 2); StringBuilder sb = new StringBuilder(); for (Map.Entry<String, Integer> e : m.entrySet()) sb.append(e.getKey()).append(e.getValue()); return sb.toString();", "\"a1b2\""),
            ("return Optional.ofNullable(null).orElse(\"dflt\");", "\"dflt\""),
            ("return Objects.equals(null, null) && Objects.isNull(null);", "true"),
            ("return Collections.max(List.of(4, 9, 2));", "9"),
            ("return Arrays.stream(new int[]{1, 2, 3}).map(x -> x * x).sum();", "14"),
            ("List<Integer> l = new ArrayList<>(); l.add(5); l.add(6); l.removeIf(x -> x == 5); return l;", "[6]"),
            ("Set<String> s = new HashSet<>(); s.add(\"a\"); s.add(\"a\"); return s.size();", "1"),
        ],
    );
}

#[test]
fn java_more() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "java",
        &ex,
        &[
            ("return List.of(\"bb\", \"a\").stream().sorted(Comparator.comparing(String::length)).collect(Collectors.toList());", "[\"a\",\"bb\"]"),
            ("return Stream.of(\"a\", \"b\").map(String::toUpperCase).toList();", "[\"A\",\"B\"]"),
            ("Object o = 5; String r = switch (o) { case Integer i -> \"int \" + i; case String s -> \"str\"; default -> \"x\"; }; return r;", "\"int 5\""),
            ("int x = 0; x += 5; x *= 2; return x;", "10"),
            ("return exchange.getMessage().getHeader(\"foo\", String.class);", "\"bar\""),
            ("return body instanceof String;", "true"),
            ("String s = null; return s == null ? \"null\" : s;", "\"null\""),
            ("Map<String, Integer> m = Map.of(\"a\", 1); int t = 0; for (String k : m.keySet()) t += m.get(k); return t;", "1"),
            ("List<Integer> nums = List.of(1, 2, 3, 4, 5); return nums.stream().filter(n -> n > 2).count();", "3"),
            ("String text = \"\"\"\n    Hello\n      World\n    \"\"\"; return text;", "\"Hello\\n  World\\n\""),
            ("return String.valueOf(body).toLowerCase().split(\" \")[1];", "\"world\""),
            ("Integer i = null; return Optional.ofNullable(i).map(v -> v * 2).orElseGet(() -> -1);", "-1"),
            ("return new int[][]{{1, 2}, {3}}[1][0];", "3"),
            ("var sb = new StringBuilder(\"ab\"); return sb.reverse().toString();", "\"ba\""),
            ("return Integer.toHexString(255) + Long.parseLong(\"7\");", "\"ff7\""),
            ("int i = 0; i++; ++i; return i--;", "2"),
            ("String r = \"\"; for (char ch : \"abc\".toCharArray()) r = ch + r; return r;", "\"cba\""),
            ("switch (header(\"foo\").toString()) { case \"bar\" -> { return \"matched\"; } default -> { return \"no\"; } }", "\"matched\""),
        ],
    );
}

#[test]
fn java_processor_mutates_exchange() {
    let ctx = ctx();
    let mut ex = exchange(&ctx);
    let p = cameleer_script::script("java", "exchange.getMessage().setHeader(\"len\", body.toString().length()); message.setBody(((String) body).toUpperCase());").unwrap();
    cameleer_core::spi::Processor::process(&p, &mut ex).unwrap();
    assert_eq!(ex.message.body.to_string(), "HELLO WORLD");
    assert_eq!(ex.message.headers.get("len").map(|v| v.to_json()), Some("11".into()));
}
