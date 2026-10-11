## Measured results (machine: 16 CPUs, 15.7 GB RAM, Windows-10-10.0.26200-SP0)


| Language | Version | Parity (mismatches / 20,000) | Parse ms | Rules 1 core ms | Rules 16 cores ms | Batch peak MB | Build s | Cold start s |
|---|---|---|---|---|---|---|---|---|
| Rust | rustc 1.99 | 0 | 131 | 42 | 6 | 96 | 89 | 0.53 |
| Go | go1.27.0 | 0 | 340 | 82 | 22 | 126 | 17 | 0.54 |
| C#/.NET | dotnet 10.0.10 | 0 | 644 | 61 | 16 | 198 | 4 | 0.64 |
| Java | 27 | 0 | 401 | 60 | 12 | 321 | 1 | 1.23 |
| TypeScript/Node | v24.13.0 | 0 | 1094 | 272 | 114 | 1024 | 0 | 1.12 |
| Python (now) | 3.10.11 | 0 | 434 | 3492 | 203 | 664 | 0 | 2.22 |

| Language | 1 conn: median req/s [min-max over 3 passes] (p99 ms) | 16 conn: median req/s [min-max over 3 passes] (p99 ms) | 64 conn: median req/s [min-max over 3 passes] (p99 ms) | 256 conn: median req/s [min-max over 3 passes] (p99 ms) | Errors | Wrong answers | Server MB at 64 | Cores used at 64 |
|---|---|---|---|---|---|---|---|---|
| Rust | 3,438 [3,400-8,951] (1.0) | 24,450 [23,724-52,728] (3.6) | 27,203 [24,916-60,511] (10.7) | 25,846 [25,512-60,765] (41.7) | 0 | 0 | 7 | 3.9 |
| Go | 4,355 [2,626-4,707] (0.8) | 16,834 [12,451-21,141] (3.4) | 14,331 [14,176-25,890] (15.8) | 18,897 [18,578-38,782] (84.8) | 0 | 0 | 22 | 4.3 |
| C#/.NET | 2,487 [2,459-2,649] (1.1) | 21,350 [21,053-22,212] (2.5) | 25,095 [22,526-27,405] (10.4) | 26,949 [26,296-27,160] (40.8) | 0 | 0 | 66 | 3.8 |
| Java | 2,256 [2,249-2,309] (1.4) | 19,860 [18,007-20,083] (3.3) | 22,957 [21,941-23,103] (11.6) | 23,900 [20,453-24,666] (43.9) | 0 | 0 | 283 | 4.2 |
| TypeScript/Node | 1,722 [1,668-4,268] (2.1) | 9,265 [9,116-27,823] (6.1) | 10,048 [9,436-28,088] (35.3) | 10,559 [9,817-31,594] (205.5) | 0 | 0 | 1168 | 5.9 |
| Python (now) | 644 [638-1,698] (3.3) | 4,162 [4,131-9,677] (10.9) | 4,539 [4,291-8,726] (41.6) | 4,786 [4,627-7,192] (218.4) | 0 | 0 | 720 | 6.4 |

| Language | Hostile requests handled cleanly (of 22) | Crashed or dropped |
|---|---|---|
| Rust | 22 | none |
| Go | 22 | none |
| C#/.NET | 22 | none |
| Java | 21 | 2 MB body |
| TypeScript/Node | 21 | 2 MB body |
| Python (now) | 22 | none |

| Criterion | Rust | Go | C#/.NET | Java | TypeScript/Node | Python (now) |
|---|---|---|---|---|---|---|
| Raw speed | 10.0 | 8.5 | 9.2 | 9.2 | 5.8 | 0.0 |
| Traffic handling | 10.0 | 7.3 | 9.9 | 9.3 | 4.6 | 0.0 |
| Tail latency | 9.9 | 5.6 | 10.0 | 9.6 | 0.4 | 0.0 |
| Memory and CPU efficiency | 10.0 | 7.2 | 7.4 | 5.5 | 1.4 | 0.0 |
| All-cores batch throughput | 10.0 | 6.4 | 7.3 | 8.1 | 1.7 | 0.0 |
| Correct and clean under traffic | 10.0 | 10.0 | 10.0 | 9.5 | 9.5 | 10.0 |
| Bug resistance by design | 9.0 | 6.0 | 7.0 | 6.0 | 5.0 | 4.0 |
| Delivery fit for this project | 3.0 | 4.0 | 3.0 | 3.0 | 4.0 | 10.0 |
| **Total, balanced** | **8.7** | **6.9** | **7.7** | **7.2** | **4.6** | **3.8** |
| **Total, performance first** | **9.9** | **7.3** | **8.8** | **8.4** | **3.8** | **0.7** |
| **Total, safety first** | **9.3** | **7.4** | **8.3** | **7.7** | **5.7** | **4.9** |
| **Total, delivery first** | **6.4** | **5.7** | **5.9** | **5.6** | **4.4** | **6.4** |
