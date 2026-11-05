// kfake-server runs franz-go's kfake (a fake Kafka cluster written by the author of the franz-go
// client) as a standalone server for Cameleer's simulator suite (scripts/simulators.sh).
package main

import (
	"crypto/tls"
	"flag"
	"log"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"

	"github.com/twmb/franz-go/pkg/kfake"
)

func main() {
	ports := flag.String("ports", "19092", "comma separated broker ports (one broker per port)")
	cert := flag.String("tls-cert", "", "PEM certificate (enables TLS)")
	key := flag.String("tls-key", "", "PEM private key")
	sasl := flag.String("sasl", "", "user:password superuser (enables SASL PLAIN / SCRAM-SHA-256 / SCRAM-SHA-512)")
	partitions := flag.Int("partitions", 3, "default partitions of auto-created topics")
	flag.Parse()

	var ps []int
	for _, p := range strings.Split(*ports, ",") {
		n, err := strconv.Atoi(strings.TrimSpace(p))
		if err != nil {
			log.Fatalf("bad port %q", p)
		}
		ps = append(ps, n)
	}
	opts := []kfake.Opt{kfake.NumBrokers(len(ps)), kfake.Ports(ps...), kfake.AllowAutoTopicCreation(), kfake.DefaultNumPartitions(*partitions)}
	if *cert != "" {
		c, err := tls.LoadX509KeyPair(*cert, *key)
		if err != nil {
			log.Fatal(err)
		}
		opts = append(opts, kfake.TLS(&tls.Config{Certificates: []tls.Certificate{c}}))
	}
	if *sasl != "" {
		up := strings.SplitN(*sasl, ":", 2)
		opts = append(opts, kfake.EnableSASL(),
			kfake.Superuser("PLAIN", up[0], up[1]),
			kfake.Superuser("SCRAM-SHA-256", up[0], up[1]),
			kfake.Superuser("SCRAM-SHA-512", up[0], up[1]))
	}
	c, err := kfake.NewCluster(opts...)
	if err != nil {
		log.Fatal(err)
	}
	log.Printf("kfake listening on %v", c.ListenAddrs())
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	c.Close()
}
