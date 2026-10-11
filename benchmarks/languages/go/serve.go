package main

import (
	"encoding/json"
	"io"
	"net/http"
)

// serveHTTP: POST /evaluate with one claim as JSON, answers {"claim_id": ..., "statuses": "PPFU..."}.
func serveHTTP(p *Pack, addr string) {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /evaluate", func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(io.LimitReader(r.Body, 1<<20))
		if err != nil {
			http.Error(w, "read", 400)
			return
		}
		var c Claim
		if err := json.Unmarshal(body, &c); err != nil {
			http.Error(w, "bad claim", 400)
			return
		}
		var out [15]byte
		p.Evaluate(&c, &out)
		w.Header().Set("Content-Type", "application/json")
		resp, _ := json.Marshal(map[string]string{"claim_id": c.ClaimID, "statuses": string(out[:])})
		w.Write(resp)
	})
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, r *http.Request) { w.Write([]byte("ok")) })
	if err := http.ListenAndServe(addr, mux); err != nil {
		panic(err)
	}
}
