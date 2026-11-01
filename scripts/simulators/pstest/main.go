// pstest-server runs cloud.google.com/go/pubsub's pstest (the in-memory Pub/Sub gRPC fake that
// ships with Google's Go client library) as a standalone server for scripts/simulators.sh.
package main

import (
	"flag"
	"log"
	"os"
	"os/signal"
	"syscall"

	"cloud.google.com/go/pubsub/pstest"
)

func main() {
	port := flag.Int("port", 18085, "gRPC port")
	flag.Parse()
	srv := pstest.NewServerWithPort(*port)
	log.Printf("pstest listening on %s", srv.Addr)
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	srv.Close()
}
