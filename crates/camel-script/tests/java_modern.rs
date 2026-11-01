//! Java (jOOR): class / record / enum / interface declarations, `char`, overloads by static
//! type, generics, sorted collections, streams, `Optional`, `String.format`, `StringBuilder`
//! and `java.time`.

mod common;
use common::*;

#[test]
fn java_declarations() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "java",
        &ex,
        &[
            ("class P { int x; P(int x) { this.x = x; } int twice() { return x * 2; } }\nreturn new P(4).twice();", "8"),
            ("record Pt(int x, int y) { int sum() { return x + y; } }\nvar p = new Pt(1, 2);\nreturn p.x() + p.sum() + p.toString();", "\"4Pt[x=1, y=2]\""),
            ("record Money(long cents, String currency) { Money { if (cents < 0) throw new IllegalArgumentException(\"negative\"); } }\ntry { new Money(-1, \"EUR\"); return \"no\"; } catch (IllegalArgumentException e) { return e.getMessage(); }", "\"negative\""),
            ("record R(String a) {}\nreturn new R(\"x\").equals(new R(\"x\")) && !new R(\"x\").equals(new R(\"y\"));", "true"),
            ("enum Color { RED, GREEN, BLUE; }\nreturn Color.GREEN.ordinal() + Color.valueOf(\"RED\").name() + Color.values().length;", "\"1RED3\""),
            ("enum Planet { MERCURY(3.3e23), EARTH(5.97e24); final double mass; Planet(double m) { this.mass = m; } double mass() { return mass; } }\nreturn Planet.EARTH.mass() > Planet.MERCURY.mass();", "true"),
            ("enum Op { ADD, SUB }\nOp o = Op.SUB;\nreturn switch (o) { case ADD -> 1; case SUB -> -1; };", "-1"),
            ("interface Shape { double area(); default String kind() { return \"shape\"; } }\nclass Sq implements Shape { double s; Sq(double s) { this.s = s; } public double area() { return s * s; } }\nShape q = new Sq(1.5);\nreturn q.area() + q.kind();", "\"2.25shape\""),
            ("abstract class Animal { abstract String sound(); String speak() { return \"I say \" + sound(); } }\nclass Dog extends Animal { String sound() { return \"woof\"; } }\nreturn new Dog().speak();", "\"I say woof\""),
            ("class Counter { private static int count = 0; static int next() { return ++count; } }\nCounter.next(); Counter.next();\nreturn Counter.next();", "3"),
            ("static int fact(int n) { return n <= 1 ? 1 : n * fact(n - 1); }\nreturn fact(5);", "120"),
            ("static String describe(int x) { return \"int\"; }\nstatic String describe(String s) { return \"string\"; }\nstatic String describe(List<?> l) { return \"list\"; }\nreturn describe(1) + describe(\"a\") + describe(List.of());", "\"intstringlist\""),
            ("class Box<T> { private T value; Box(T v) { value = v; } T get() { return value; } <R> Box<R> map(java.util.function.Function<T, R> f) { return new Box<>(f.apply(value)); } }\nreturn new Box<>(21).map(x -> x * 2).get();", "42"),
            ("static <T extends Comparable<T>> T max(List<T> xs) { T best = xs.get(0); for (T x : xs) if (x.compareTo(best) > 0) best = x; return best; }\nreturn max(List.of(3, 9, 4));", "9"),
            ("List<String> raw = new ArrayList<>();\nList erased = raw;\nerased.add(1);\nreturn raw.size();", "1"),
        ],
    );
}

#[test]
fn java_char_and_overloads() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "java",
        &ex,
        &[
            ("char c = 'a';\nc++;\nchar d = (char) (c + 1);\nreturn \"\" + c + d + (int) c + (char) 65;", "\"bc98A\""),
            ("int[] counts = new int[26];\nfor (char ch : \"hello\".toCharArray()) counts[ch - 'a']++;\nreturn counts['l' - 'a'];", "2"),
            ("String s = \"a1b2\"; int sum = 0;\nfor (int i = 0; i < s.length(); i++) { char ch = s.charAt(i); if (Character.isDigit(ch)) sum += ch - '0'; }\nreturn sum;", "3"),
            ("char c = 'x'; c += 2; return c;", "\"z\""),
            ("char up = (char) ('a' - 32); return up;", "\"A\""),
            ("int code = 'A'; char back = (char) (code + 25); return code + \":\" + back;", "\"65:Z\""),
            ("char c = 'm'; return c >= 'a' && c <= 'z' ? \"lower\" : \"other\";", "\"lower\""),
            ("StringBuilder sb = new StringBuilder(); for (char c = 'a'; c <= 'e'; c++) sb.append(c); return sb.toString();", "\"abcde\""),
            ("List<Integer> l = new ArrayList<>(List.of(10, 20, 30));\nl.remove(Integer.valueOf(10));\nl.remove(0);\nreturn l;", "[30]"),
            ("List<Integer> l = new ArrayList<>(List.of(5, 6, 7));\nInteger boxed = 7;\nboolean removed = l.remove(boxed);\nint idx = 0;\nl.remove(idx);\nreturn removed + \" \" + l;", "\"true [6]\""),
            ("List<Integer> l = new ArrayList<>(List.of(1, 2, 3));\nl.remove((Object) 3);\nreturn l;", "[1,2]"),
        ],
    );
}

#[test]
fn java_collections_streams_text() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "java",
        &ex,
        &[
            ("TreeMap<String, Integer> m = new TreeMap<>();\nm.put(\"b\", 2); m.put(\"a\", 1); m.put(\"c\", 3);\nreturn m.firstKey() + m.keySet() + m.lastEntry().getValue();", "\"a[a, b, c]3\""),
            ("TreeMap<Integer, String> m = new TreeMap<>(Map.of(10, \"x\", 2, \"y\", 33, \"z\"));\nreturn m.keySet() + \" \" + m.floorKey(20) + \" \" + m.ceilingKey(20) + \" \" + m.headMap(10).size();", "\"[2, 10, 33] 10 33 1\""),
            ("TreeSet<Integer> s = new TreeSet<>(); s.add(5); s.add(1); s.add(3);\nreturn s.first() + \":\" + s + \":\" + s.higher(3) + \":\" + s.pollFirst() + \":\" + s;", "\"1:[1, 3, 5]:5:1:[3, 5]\""),
            ("TreeSet<String> s = new TreeSet<>(Comparator.reverseOrder()); s.addAll(List.of(\"a\", \"c\", \"b\"));\nreturn String.join(\",\", s);", "\"c,b,a\""),
            ("TreeMap<String, Integer> m = new TreeMap<>((a, b) -> b.length() - a.length());\nm.put(\"aa\", 1); m.put(\"a\", 2); m.put(\"aaa\", 3);\nreturn m.keySet();", "[\"aaa\",\"aa\",\"a\"]"),
            ("return List.of(1, 2, 3, 4).stream().filter(x -> x % 2 == 0).map(x -> x * 10).collect(Collectors.toList());", "[20,40]"),
            ("return Stream.of(\"a\", \"bb\", \"cc\").collect(Collectors.groupingBy(String::length));", "{\"1\":[\"a\"],\"2\":[\"bb\",\"cc\"]}"),
            ("return Stream.of(\"x\", \"y\", \"z\").collect(Collectors.joining(\", \", \"[\", \"]\"));", "\"[x, y, z]\""),
            ("return IntStream.rangeClosed(1, 5).sum();", "15"),
            ("return Optional.ofNullable(null).map(x -> \"v\").orElse(\"none\") + Optional.of(3).map(x -> x * 2).get();", "\"none6\""),
            ("return String.format(\"%05.1f|%-4s|%d|%x|%,d\", 3.14159, \"ab\", 42, 255, 1234567);", "\"003.1|ab  |42|ff|1,234,567\""),
            ("StringBuilder sb = new StringBuilder(); sb.append(\"ab\").append(1).insert(0, \"x\").reverse(); return sb.toString() + sb.length();", "\"1bax4\""),
        ],
    );
}

#[test]
fn java_time() {
    let ctx = ctx();
    let ex = exchange(&ctx);
    check(
        "java",
        &ex,
        &[
            ("return LocalDate.of(2024, 1, 31).plusMonths(1).toString();", "\"2024-02-29\""),
            ("LocalDate d = LocalDate.parse(\"2024-03-15\");\nreturn d.getYear() + \"/\" + d.getMonthValue() + \"/\" + d.getDayOfMonth() + \" \" + d.getDayOfWeek() + \" \" + d.getDayOfYear() + \" \" + d.isLeapYear();", "\"2024/3/15 FRIDAY 75 true\""),
            ("return LocalDateTime.of(2024, 1, 5, 10, 30).plusHours(20).getDayOfWeek();", "\"SATURDAY\""),
            ("return LocalDateTime.of(2024, 1, 5, 10, 30).toString() + \" \" + LocalDateTime.of(2024, 1, 5, 10, 30, 15).toString();", "\"2024-01-05T10:30 2024-01-05T10:30:15\""),
            ("LocalDate a = LocalDate.of(2024, 1, 1), b = LocalDate.of(2024, 3, 1);\nreturn ChronoUnit.DAYS.between(a, b) + \" \" + ChronoUnit.MONTHS.between(a, b) + \" \" + a.isBefore(b) + \" \" + Period.between(a, b);", "\"60 2 true P2M\""),
            ("DateTimeFormatter f = DateTimeFormatter.ofPattern(\"dd/MM/yyyy HH:mm\");\nreturn LocalDateTime.of(2024, 7, 4, 9, 5).format(f);", "\"04/07/2024 09:05\""),
            ("return LocalDate.parse(\"05.01.2024\", DateTimeFormatter.ofPattern(\"dd.MM.yyyy\")).plusWeeks(1).toString();", "\"2024-01-12\""),
            ("Duration d = Duration.between(LocalDateTime.of(2024, 1, 1, 8, 0), LocalDateTime.of(2024, 1, 1, 17, 30));\nreturn d.toMinutes() + \" \" + d;", "\"570 PT9H30M\""),
            ("return LocalTime.of(23, 30).plusMinutes(45).toString() + \" \" + LocalTime.parse(\"10:15:30\").getSecond();", "\"00:15 30\""),
            ("LocalDate d = LocalDate.of(2024, 2, 10);\nreturn d.withDayOfMonth(1) + \" \" + d.withDayOfMonth(d.lengthOfMonth()) + \" \" + d.atStartOfDay() + \" \" + d.atTime(8, 30);", "\"2024-02-01 2024-02-29 2024-02-10T00:00 2024-02-10T08:30\""),
            ("return LocalDate.of(2024, 1, 1).getDayOfWeek() == DayOfWeek.MONDAY ? \"monday\" : \"no\";", "\"monday\""),
            ("List<LocalDate> ds = new ArrayList<>(List.of(LocalDate.of(2024, 5, 1), LocalDate.of(2023, 1, 1), LocalDate.of(2024, 1, 1)));\nCollections.sort(ds);\nreturn ds.get(0) + \" \" + LocalDate.of(2024, 1, 1).equals(LocalDate.parse(\"2024-01-01\"));", "\"2023-01-01 true\""),
        ],
    );
}
