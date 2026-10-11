package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"sync"
	"time"
)

func median(xs []float64) float64 {
	s := append([]float64(nil), xs...)
	sort.Float64s(s)
	return s[len(s)/2]
}

func evalAll(p *Pack, claims []Claim, out [][15]byte, threads int) {
	if threads <= 1 {
		for i := range claims {
			p.Evaluate(&claims[i], &out[i])
		}
		return
	}
	var wg sync.WaitGroup
	chunk := (len(claims) + threads - 1) / threads
	for t := 0; t < threads; t++ {
		lo, hi := t*chunk, (t+1)*chunk
		if hi > len(claims) {
			hi = len(claims)
		}
		if lo >= hi {
			break
		}
		wg.Add(1)
		go func(lo, hi int) {
			defer wg.Done()
			for i := lo; i < hi; i++ {
				p.Evaluate(&claims[i], &out[i])
			}
		}(lo, hi)
	}
	wg.Wait()
}

func main() {
	corpus := flag.String("corpus", "", "corpus directory")
	threads := flag.Int("threads", runtime.NumCPU(), "worker threads")
	repeat := flag.Int("repeat", 5, "timed repeats")
	serve := flag.String("serve", "", "listen address for HTTP mode, e.g. 127.0.0.1:9101")
	flag.Parse()

	pol, err := os.ReadFile(filepath.Join(*corpus, "pack", "policies.json"))
	check(err)
	svc, err := os.ReadFile(filepath.Join(*corpus, "pack", "services.json"))
	check(err)
	pack, err := NewPack(pol, svc)
	check(err)
	if *serve != "" {
		serveHTTP(pack, *serve)
		return
	}

	t0 := time.Now()
	raw, err := os.ReadFile(filepath.Join(*corpus, "claims.jsonl"))
	check(err)
	lines := bytes.Split(bytes.TrimRight(raw, "\n"), []byte("\n"))
	readMs := float64(time.Since(t0).Microseconds()) / 1000

	t0 = time.Now()
	claims := make([]Claim, len(lines))
	for i, l := range lines {
		check(json.Unmarshal(l, &claims[i]))
	}
	parseMs := float64(time.Since(t0).Microseconds()) / 1000

	out := make([][15]byte, len(claims))
	evalAll(pack, claims, out, 1) // warm-up and parity run

	exp, err := os.Open(filepath.Join(*corpus, "expected.txt"))
	check(err)
	sc := bufio.NewScanner(exp)
	mismatch, n := 0, 0
	for sc.Scan() {
		if n < len(out) && sc.Text() != string(out[n][:]) {
			mismatch++
		}
		n++
	}

	var one, many []float64
	for r := 0; r < *repeat; r++ {
		t := time.Now()
		evalAll(pack, claims, out, 1)
		one = append(one, float64(time.Since(t).Microseconds())/1000)
		t = time.Now()
		evalAll(pack, claims, out, *threads)
		many = append(many, float64(time.Since(t).Microseconds())/1000)
	}
	res := map[string]any{
		"lang": "go", "version": runtime.Version(), "claims": len(claims), "mismatches": mismatch, "threads": *threads,
		"read_ms": readMs, "parse_ms": parseMs, "eval_1t_ms": median(one), "eval_mt_ms": median(many),
	}
	b, _ := json.Marshal(res)
	fmt.Println(string(b))
}

func check(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}
