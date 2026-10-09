// loadgen drives POST /evaluate on any of the servers with real claims and checks every answer against the oracle.
//
//	loadgen -mode load  -url http://127.0.0.1:9101 -corpus ../corpus -conc 64 -dur 10s
//	loadgen -mode probe -url http://127.0.0.1:9101 -corpus ../corpus
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

func readLines(path string, max int) []string {
	f, err := os.Open(path)
	if err != nil {
		panic(err)
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<24)
	var out []string
	for sc.Scan() && len(out) < max {
		out = append(out, sc.Text())
	}
	return out
}

func main() {
	mode := flag.String("mode", "load", "load | probe")
	url := flag.String("url", "", "server base url")
	corpus := flag.String("corpus", "", "corpus dir")
	conc := flag.Int("conc", 64, "concurrent connections")
	dur := flag.Duration("dur", 10*time.Second, "duration")
	flag.Parse()
	claims := readLines(filepath.Join(*corpus, "claims.jsonl"), 4000)
	exp := readLines(filepath.Join(*corpus, "expected.txt"), 4000)
	if *mode == "probe" {
		probe(*url, claims)
		return
	}
	tr := &http.Transport{MaxIdleConnsPerHost: *conc, MaxConnsPerHost: *conc, DisableCompression: true}
	client := &http.Client{Transport: tr, Timeout: 30 * time.Second}

	// warm-up: 2 seconds at the same concurrency, not measured
	run(client, *url, claims, exp, *conc, 2*time.Second)
	res := run(client, *url, claims, exp, *conc, *dur)
	b, _ := json.Marshal(res)
	fmt.Println(string(b))
}

func run(client *http.Client, url string, claims, exp []string, conc int, dur time.Duration) map[string]any {
	var mu sync.Mutex
	var lat []float64
	var errs, mism, total int64
	var firstErr atomic.Value
	var next int64
	stop := time.Now().Add(dur)
	var wg sync.WaitGroup
	start := time.Now()
	for w := 0; w < conc; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			local := make([]float64, 0, 4096)
			for time.Now().Before(stop) {
				i := int(atomic.AddInt64(&next, 1)) % len(claims)
				t := time.Now()
				resp, err := client.Post(url+"/evaluate", "application/json", strings.NewReader(claims[i]))
				if err != nil {
					atomic.AddInt64(&errs, 1)
					firstErr.CompareAndSwap(nil, err.Error())
					continue
				}
				body, _ := io.ReadAll(resp.Body)
				resp.Body.Close()
				local = append(local, float64(time.Since(t).Microseconds())/1000)
				atomic.AddInt64(&total, 1)
				if resp.StatusCode != 200 {
					atomic.AddInt64(&errs, 1)
					continue
				}
				if !bytes.Contains(body, []byte(`"statuses":"`+exp[i]+`"`)) {
					atomic.AddInt64(&mism, 1)
				}
			}
			mu.Lock()
			lat = append(lat, local...)
			mu.Unlock()
		}()
	}
	wg.Wait()
	el := time.Since(start).Seconds()
	sort.Float64s(lat)
	pct := func(p float64) float64 {
		if len(lat) == 0 {
			return 0
		}
		return lat[int(float64(len(lat)-1)*p)]
	}
	fe, _ := firstErr.Load().(string)
	return map[string]any{"first_error": fe, "conc": conc, "seconds": el, "requests": total, "rps": float64(total) / el, "errors": errs, "mismatches": mism,
		"p50_ms": pct(0.5), "p95_ms": pct(0.95), "p99_ms": pct(0.99), "max_ms": pct(1)}
}

// probe sends hostile and malformed bodies. A good server answers 4xx (or 200 where the claim is still evaluable) and stays up.
func probe(url string, claims []string) {
	var c map[string]any
	json.Unmarshal([]byte(claims[0]), &c)
	mut := func(f func(m map[string]any)) string {
		var m map[string]any
		json.Unmarshal([]byte(claims[0]), &m)
		f(m)
		b, _ := json.Marshal(m)
		return string(b)
	}
	lines := func(m map[string]any) []any { return m["lines"].([]any) }
	type tc struct {
		name, body string
	}
	deep := strings.Repeat("[", 20000) + strings.Repeat("]", 20000)
	cases := []tc{
		{"empty body", ""},
		{"not json", "hello"},
		{"truncated json", claims[0][:len(claims[0])/2]},
		{"json array instead of object", "[]"},
		{"json null", "null"},
		{"empty object", "{}"},
		{"quantity is a string", mut(func(m map[string]any) { lines(m)[0].(map[string]any)["quantity"] = "two" })},
		{"unit_price is an object", mut(func(m map[string]any) { lines(m)[0].(map[string]any)["unit_price"] = map[string]any{"a": 1} })},
		{"lines is a string", mut(func(m map[string]any) { m["lines"] = "none" })},
		{"lines is empty", mut(func(m map[string]any) { m["lines"] = []any{} })},
		{"policy_id is a number", mut(func(m map[string]any) { m["policy_id"] = 7 })},
		{"coverage is null", mut(func(m map[string]any) { m["coverage"] = nil })},
		{"authorizations is null", mut(func(m map[string]any) { m["authorizations"] = nil })},
		{"huge number", mut(func(m map[string]any) {
			lines(m)[0].(map[string]any)["net_amount"] = json.Number("1" + strings.Repeat("0", 400))
		})},
		{"negative zero and exponent", `{"claim_id":"X","lines":[{"quantity":-0.0,"unit_price":1e5,"net_amount":1E-3}]}`},
		{"nested 20000 deep", deep},
		{"nul bytes", "{\"claim_id\":\"a\x00b\"}"},
		{"invalid utf-8", "{\"claim_id\":\"\xff\xfe\"}"},
		{"unicode whitespace id", mut(func(m map[string]any) { m["invoice_number"] = "  " })},
		{"duplicate keys", `{"claim_id":"a","claim_id":"b","lines":[]}`},
		{"2 MB body", `{"claim_id":"` + strings.Repeat("a", 2<<20) + `"}`},
		{"10000 lines", mut(func(m map[string]any) {
			l := lines(m)[0]
			big := make([]any, 10000)
			for i := range big {
				big[i] = l
			}
			m["lines"] = big
		})},
	}
	client := &http.Client{Timeout: 10 * time.Second}
	results := []map[string]any{}
	for _, t := range cases {
		st := time.Now()
		resp, err := client.Post(url+"/evaluate", "application/json", strings.NewReader(t.body))
		r := map[string]any{"case": t.name}
		if err != nil {
			r["status"] = "transport-error"
			r["error"] = err.Error()
		} else {
			io.Copy(io.Discard, resp.Body)
			resp.Body.Close()
			r["status"] = resp.StatusCode
		}
		r["ms"] = float64(time.Since(st).Microseconds()) / 1000
		results = append(results, r)
	}
	alive := false
	resp, err := client.Get(url + "/healthz")
	if err == nil {
		io.Copy(io.Discard, resp.Body)
		resp.Body.Close()
		alive = resp.StatusCode == 200
	}
	b, _ := json.Marshal(map[string]any{"cases": results, "alive_after": alive})
	fmt.Println(string(b))
}
