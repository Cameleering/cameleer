// pstest-server runs cloud.google.com/go/pubsub's pstest (the in-memory Pub/Sub gRPC fake that
// ships with Google's Go client library) as a standalone server for scripts/simulators.sh.
//
// Besides the gRPC port it serves a small HTTP control API (pstest's own Go methods) for the
// tests, since rust-camel publishes over REST, which pstest does not speak:
//
//	POST /topic?name=projects/p/topics/t
//	POST /subscription?name=projects/p/subscriptions/s&topic=projects/p/topics/t&ackDeadline=10
//	POST /publish?topic=projects/p/topics/t&attr.k=v   (body = data) -> message id
//	GET  /messages?topic=projects/p/topics/t           -> [{id, data, attributes, acks, deliveries}]
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"syscall"

	"cloud.google.com/go/pubsub/apiv1/pubsubpb"
	"cloud.google.com/go/pubsub/pstest"
)

func main() {
	port := flag.Int("port", 18085, "gRPC port")
	control := flag.String("control", "127.0.0.1:18086", "HTTP control address")
	flag.Parse()
	srv := pstest.NewServerWithPort(*port)
	log.Printf("pstest listening on %s, control on %s", srv.Addr, *control)

	fail := func(w http.ResponseWriter, err error) { http.Error(w, err.Error(), http.StatusBadRequest) }
	http.HandleFunc("/topic", func(w http.ResponseWriter, r *http.Request) {
		if _, err := srv.GServer.CreateTopic(context.Background(), &pubsubpb.Topic{Name: r.URL.Query().Get("name")}); err != nil {
			fail(w, err)
		}
	})
	http.HandleFunc("/subscription", func(w http.ResponseWriter, r *http.Request) {
		q := r.URL.Query()
		deadline, _ := strconv.Atoi(q.Get("ackDeadline"))
		if deadline == 0 {
			deadline = 10
		}
		sub := &pubsubpb.Subscription{Name: q.Get("name"), Topic: q.Get("topic"), AckDeadlineSeconds: int32(deadline)}
		if _, err := srv.GServer.CreateSubscription(context.Background(), sub); err != nil {
			fail(w, err)
		}
	})
	http.HandleFunc("/publish", func(w http.ResponseWriter, r *http.Request) {
		data, _ := io.ReadAll(r.Body)
		attrs := map[string]string{}
		for k, v := range r.URL.Query() {
			if strings.HasPrefix(k, "attr.") {
				attrs[strings.TrimPrefix(k, "attr.")] = v[0]
			}
		}
		fmt.Fprint(w, srv.Publish(r.URL.Query().Get("topic"), data, attrs))
	})
	http.HandleFunc("/messages", func(w http.ResponseWriter, r *http.Request) {
		type msg struct {
			ID         string            `json:"id"`
			Data       string            `json:"data"`
			Attributes map[string]string `json:"attributes"`
			Acks       int               `json:"acks"`
			Deliveries int               `json:"deliveries"`
		}
		out := []msg{}
		for _, m := range srv.Messages() {
			out = append(out, msg{m.ID, string(m.Data), m.Attributes, m.Acks, m.Deliveries})
		}
		_ = json.NewEncoder(w).Encode(out)
	})
	go func() { log.Fatal(http.ListenAndServe(*control, nil)) }()

	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	srv.Close()
}
