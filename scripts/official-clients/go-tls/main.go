// Go's crypto/tls and net/http (HTTP/2) against Cameleer's TLS and HTTP/2 stacks.
//
//	go-tls probe -addr host:port [-servername n] -ca ca.pem [-min 1.2] [-max 1.3] [-suite NAME]
//	             [-curve X25519|P256|P384|X25519MLKEM768] [-cert c.pem -key c.key] [-alpn h2,http/1.1]
//	             [-conns 2]
//	    Opens -conns connections (one shared session cache: later ones resume) to a line-echo
//	    server, sends "hello\n", prints the negotiated parameters and the server's reply line.
//	go-tls h2 -url https://host:port -ca ca.pem [-servername n] [-cert c.pem -key c.key] [-n 16]
//	    net/http client (HTTP/2 over TLS with ALPN h2): GET /hello, POST /echo (1 MiB),
//	    n concurrent GETs on one connection, and an HTTP/1.1-only client for comparison.
//	go-tls serve -portfile f -cert c.pem -key c.key [-rsacert r.pem -rsakey r.key] [-ca ca.pem]
//	    net/http server (HTTP/2 + HTTP/1.1 over TLS) with /info, /echo, /stream; client
//	    certificates are verified when sent; runs until stdin is closed.
package main

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"flag"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"runtime"
	"strings"
	"sync"
	"time"
)

func die(format string, a ...interface{}) {
	fmt.Fprintf(os.Stderr, format+"\n", a...)
	os.Exit(2)
}

func pool(path string) *x509.CertPool {
	p := x509.NewCertPool()
	data, err := os.ReadFile(path)
	if err != nil || !p.AppendCertsFromPEM(data) {
		die("cannot load CA %s: %v", path, err)
	}
	return p
}

func version(s string) uint16 {
	switch s {
	case "1.2":
		return tls.VersionTLS12
	case "1.3":
		return tls.VersionTLS13
	}
	return 0
}

func suiteID(name string) uint16 {
	for _, s := range tls.CipherSuites() {
		if s.Name == name {
			return s.ID
		}
	}
	die("unknown suite %s", name)
	return 0
}

func curveID(name string) tls.CurveID {
	switch name {
	case "X25519":
		return tls.X25519
	case "P256":
		return tls.CurveP256
	case "P384":
		return tls.CurveP384
	case "X25519MLKEM768":
		return tls.X25519MLKEM768
	}
	die("unknown curve %s", name)
	return 0
}

func peerCN(cs tls.ConnectionState) string {
	if len(cs.PeerCertificates) == 0 {
		return "-"
	}
	return cs.PeerCertificates[0].Subject.CommonName
}

func describe(cs tls.ConnectionState) string {
	alpn := cs.NegotiatedProtocol
	if alpn == "" {
		alpn = "-"
	}
	return fmt.Sprintf("version=%s suite=%s alpn=%s resumed=%v peer=%s", tls.VersionName(cs.Version), tls.CipherSuiteName(cs.CipherSuite), alpn, cs.DidResume, peerCN(cs))
}

func clientConfig(ca, servername, cert, key string) *tls.Config {
	cfg := &tls.Config{RootCAs: pool(ca), ServerName: servername}
	if cert != "" {
		pair, err := tls.LoadX509KeyPair(cert, key)
		if err != nil {
			die("client certificate: %v", err)
		}
		cfg.Certificates = []tls.Certificate{pair}
	}
	return cfg
}

func probe(args []string) {
	fs := flag.NewFlagSet("probe", flag.ExitOnError)
	addr := fs.String("addr", "", "")
	servername := fs.String("servername", "localhost", "")
	ca := fs.String("ca", "", "")
	min := fs.String("min", "1.2", "")
	max := fs.String("max", "1.3", "")
	suite := fs.String("suite", "", "")
	curve := fs.String("curve", "", "")
	cert := fs.String("cert", "", "")
	key := fs.String("key", "", "")
	alpn := fs.String("alpn", "", "")
	conns := fs.Int("conns", 1, "")
	fs.Parse(args)
	cfg := clientConfig(*ca, *servername, *cert, *key)
	cfg.MinVersion, cfg.MaxVersion = version(*min), version(*max)
	if *suite != "" {
		cfg.CipherSuites = []uint16{suiteID(*suite)}
	}
	if *curve != "" {
		cfg.CurvePreferences = []tls.CurveID{curveID(*curve)}
	}
	if *alpn != "" {
		cfg.NextProtos = strings.Split(*alpn, ",")
	}
	cfg.ClientSessionCache = tls.NewLRUClientSessionCache(8)
	for i := 1; i <= *conns; i++ {
		conn, err := tls.DialWithDialer(&net.Dialer{Timeout: 10 * time.Second}, "tcp", *addr, cfg)
		if err != nil {
			fmt.Printf("conn%d-error: %v\n", i, err)
			continue
		}
		conn.SetDeadline(time.Now().Add(10 * time.Second))
		if _, err := conn.Write([]byte("hello\n")); err != nil {
			fmt.Printf("conn%d-error: write: %v\n", i, err)
			conn.Close()
			continue
		}
		line, err := bufio.NewReader(conn).ReadString('\n')
		if err != nil {
			fmt.Printf("conn%d-error: read: %v\n", i, err)
			conn.Close()
			continue
		}
		fmt.Printf("conn%d: %s\n", i, describe(conn.ConnectionState()))
		fmt.Printf("conn%d-server: %s\n", i, strings.TrimSpace(line))
		conn.Close()
	}
}

func h2(args []string) {
	fs := flag.NewFlagSet("h2", flag.ExitOnError)
	url := fs.String("url", "", "")
	ca := fs.String("ca", "", "")
	servername := fs.String("servername", "localhost", "")
	cert := fs.String("cert", "", "")
	key := fs.String("key", "", "")
	n := fs.Int("n", 16, "")
	fs.Parse(args)
	tr := &http.Transport{TLSClientConfig: clientConfig(*ca, *servername, *cert, *key), ForceAttemptHTTP2: true, Proxy: nil}
	client := &http.Client{Transport: tr, Timeout: 30 * time.Second}
	resp, err := client.Get(*url + "/hello?x=1")
	if err != nil {
		fmt.Printf("get-error: %v\n", err)
		return
	}
	body, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	fmt.Printf("proto: %s\n", resp.Proto)
	fmt.Printf("get: %d %s|%s\n", resp.StatusCode, resp.Header.Get("Content-Type"), strings.TrimSpace(string(body)))
	if resp.TLS != nil {
		fmt.Printf("tls: %s\n", describe(*resp.TLS))
	}
	big := bytes.Repeat([]byte("0123456789abcdef"), 65536)
	resp, err = client.Post(*url+"/echo", "application/octet-stream", bytes.NewReader(big))
	if err != nil {
		fmt.Printf("echo-error: %v\n", err)
	} else {
		got, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		a, b := sha256.Sum256(big), sha256.Sum256(got)
		fmt.Printf("echo: %d %d same=%v\n", resp.StatusCode, len(got), a == b)
	}
	var wg sync.WaitGroup
	var mu sync.Mutex
	okCount := 0
	for i := 0; i < *n; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			r, err := client.Get(fmt.Sprintf("%s/hello?i=%d", *url, i))
			if err != nil {
				return
			}
			b, _ := io.ReadAll(r.Body)
			r.Body.Close()
			if r.StatusCode == 200 && r.ProtoMajor == 2 && strings.Contains(string(b), fmt.Sprintf("i=%d", i)) {
				mu.Lock()
				okCount++
				mu.Unlock()
			}
		}(i)
	}
	wg.Wait()
	fmt.Printf("parallel: %d/%d\n", okCount, *n)
	resp, err = client.Get(*url + "/missing")
	if err == nil {
		io.Copy(io.Discard, resp.Body)
		resp.Body.Close()
		fmt.Printf("missing: %d\n", resp.StatusCode)
	}
	// an HTTP/1.1-only client (no h2 in ALPN) against the same server
	tr1 := &http.Transport{TLSClientConfig: clientConfig(*ca, *servername, *cert, *key), TLSNextProto: map[string]func(string, *tls.Conn) http.RoundTripper{}, Proxy: nil}
	resp, err = (&http.Client{Transport: tr1, Timeout: 30 * time.Second}).Get(*url + "/hello?v=1")
	if err != nil {
		fmt.Printf("h1-error: %v\n", err)
	} else {
		io.Copy(io.Discard, resp.Body)
		resp.Body.Close()
		fmt.Printf("h1: %s %d\n", resp.Proto, resp.StatusCode)
	}
}

func serve(args []string) {
	fs := flag.NewFlagSet("serve", flag.ExitOnError)
	portfile := fs.String("portfile", "", "")
	cert := fs.String("cert", "", "")
	key := fs.String("key", "", "")
	rsacert := fs.String("rsacert", "", "")
	rsakey := fs.String("rsakey", "", "")
	ca := fs.String("ca", "", "")
	fs.Parse(args)
	var certs []tls.Certificate
	for _, p := range [][2]string{{*cert, *key}, {*rsacert, *rsakey}} {
		if p[0] == "" {
			continue
		}
		pair, err := tls.LoadX509KeyPair(p[0], p[1])
		if err != nil {
			die("server certificate: %v", err)
		}
		certs = append(certs, pair)
	}
	cfg := &tls.Config{Certificates: certs, NextProtos: []string{"h2", "http/1.1"}}
	if *ca != "" {
		cfg.ClientCAs = pool(*ca)
		cfg.ClientAuth = tls.VerifyClientCertIfGiven
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/info", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		fmt.Fprintf(w, "proto=%s %s path=%s tenant=%s\n", r.Proto, describe(*r.TLS), r.URL.RequestURI(), r.Header.Get("X-Tenant"))
	})
	mux.HandleFunc("/echo", func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		w.Header().Set("Content-Type", r.Header.Get("Content-Type"))
		w.Header().Set("X-Proto", r.Proto)
		w.Write(body)
	})
	mux.HandleFunc("/stream", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		for i := 0; i < 5; i++ {
			fmt.Fprintf(w, "chunk %d\n", i)
			if f, ok := w.(http.Flusher); ok {
				f.Flush()
			}
		}
	})
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		die("listen: %v", err)
	}
	srv := &http.Server{Handler: mux, TLSConfig: cfg, ReadHeaderTimeout: 10 * time.Second}
	go srv.ServeTLS(ln, "", "")
	port := ln.Addr().(*net.TCPAddr).Port
	os.WriteFile(*portfile+".tmp", []byte(fmt.Sprint(port)), 0o644)
	os.Rename(*portfile+".tmp", *portfile)
	fmt.Printf("listening: %d\n", port)
	io.Copy(io.Discard, os.Stdin)
	srv.Close()
}

func main() {
	if len(os.Args) < 2 {
		die("usage: go-tls probe|h2|serve ...")
	}
	fmt.Printf("version: %s crypto/tls net/http\n", runtime.Version())
	switch os.Args[1] {
	case "probe":
		probe(os.Args[2:])
	case "h2":
		h2(os.Args[2:])
	case "serve":
		serve(os.Args[2:])
	default:
		die("unknown command %s", os.Args[1])
	}
}
