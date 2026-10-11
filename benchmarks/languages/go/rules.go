package main

import (
	"encoding/json"
	"strings"

	"github.com/shopspring/decimal"
)

// ---- JSON shapes (typed, the way a Go service would declare them) ---------------------------------------------------

// Num is a JSON number kept as an exact decimal; ok is false for null.
type Num struct {
	ok bool
	d  decimal.Decimal
}

func (n *Num) UnmarshalJSON(b []byte) error {
	s := string(b)
	if s == "null" {
		n.ok = false
		return nil
	}
	d, err := decimal.NewFromString(s)
	if err != nil {
		return err
	}
	n.ok, n.d = true, d
	return nil
}

type Coverage struct {
	Status      *string `json:"status"`
	Beneficiary *string `json:"beneficiary_patient_id"`
	MemberID    *string `json:"member_id"`
	StartDate   *string `json:"start_date"`
	EndDate     *string `json:"end_date"`
}

type Line struct {
	LineID          string  `json:"line_id"`
	ServiceCode     *string `json:"service_code"`
	ServiceDate     *string `json:"service_date"`
	Modifier        *string `json:"modifier"`
	Quantity        Num     `json:"quantity"`
	UnitPrice       Num     `json:"unit_price"`
	NetAmount       Num     `json:"net_amount"`
	AuthorizationID *string `json:"authorization_id"`
}

type Auth struct {
	AuthorizationID *string `json:"authorization_id"`
	PatientID       *string `json:"patient_id"`
	ServiceCode     *string `json:"service_code"`
	Status          *string `json:"status"`
	ValidFrom       *string `json:"valid_from"`
	ValidTo         *string `json:"valid_to"`
	MaxQuantity     Num     `json:"max_quantity"`
}

type Attachment struct {
	Type           *string `json:"type"`
	PatientID      *string `json:"patient_id"`
	ServiceCode    *string `json:"service_code"`
	ServiceDate    *string `json:"service_date"`
	DocumentStatus *string `json:"document_status"`
}

type Claim struct {
	ClaimID        string       `json:"claim_id"`
	InvoiceNumber  *string      `json:"invoice_number"`
	PatientID      *string      `json:"patient_id"`
	MemberID       *string      `json:"member_id"`
	ProviderID     *string      `json:"provider_id"`
	PolicyID       *string      `json:"policy_id"`
	DiagnosisCode  *string      `json:"diagnosis_code"`
	SubmissionDate *string      `json:"submission_date"`
	Currency       *string      `json:"currency"`
	TotalAmount    Num          `json:"total_amount"`
	Coverage       Coverage     `json:"coverage"`
	Lines          []Line       `json:"lines"`
	Authorizations []Auth       `json:"authorizations"`
	Attachments    []Attachment `json:"attachments"`
}

type Policy struct {
	Currency             string            `json:"currency"`
	SubmissionWindowDays int               `json:"submission_window_days"`
	AllowedProviders     []string          `json:"allowed_providers"`
	AuthRequired         []string          `json:"auth_required_services"`
	RequiredDocuments    map[string]string `json:"required_documents"`
	MaxUnitPrice         map[string]Num    `json:"max_unit_price"`
	MaxQuantityPerLine   map[string]Num    `json:"max_quantity_per_line"`
}

type Pack struct {
	Policies map[string]*Policy
	Services map[string]json.RawMessage
	allowed  map[string]map[string]bool
	authReq  map[string]map[string]bool
}

func NewPack(policiesJSON, servicesJSON []byte) (*Pack, error) {
	p := &Pack{Policies: map[string]*Policy{}, allowed: map[string]map[string]bool{}, authReq: map[string]map[string]bool{}}
	if err := json.Unmarshal(policiesJSON, &p.Policies); err != nil {
		return nil, err
	}
	if err := json.Unmarshal(servicesJSON, &p.Services); err != nil {
		return nil, err
	}
	for id, pol := range p.Policies {
		a := map[string]bool{}
		for _, x := range pol.AllowedProviders {
			a[x] = true
		}
		r := map[string]bool{}
		for _, x := range pol.AuthRequired {
			r[x] = true
		}
		p.allowed[id], p.authReq[id] = a, r
	}
	return p, nil
}

// ---- helpers --------------------------------------------------------------------------------------------------------

var cent = decimal.New(1, -2)

func empty(s *string) bool { return s == nil || strings.TrimSpace(*s) == "" }

func sval(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}

// day returns days since 0001-01-01 for a well-formed ISO date, ok=false otherwise (no trimming, no repair).
func day(s *string) (int, bool) {
	if s == nil || len(*s) != 10 {
		return 0, false
	}
	v := *s
	for i := 0; i < 10; i++ {
		if i == 4 || i == 7 {
			if v[i] != '-' {
				return 0, false
			}
		} else if v[i] < '0' || v[i] > '9' {
			return 0, false
		}
	}
	y := int(v[0]-'0')*1000 + int(v[1]-'0')*100 + int(v[2]-'0')*10 + int(v[3]-'0')
	m := int(v[5]-'0')*10 + int(v[6]-'0')
	d := int(v[8]-'0')*10 + int(v[9]-'0')
	if y < 1 || m < 1 || m > 12 || d < 1 {
		return 0, false
	}
	dim := [...]int{31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31}[m-1]
	if m == 2 && (y%4 == 0 && (y%100 != 0 || y%400 == 0)) {
		dim = 29
	}
	if d > dim {
		return 0, false
	}
	yy := y - 1
	days := yy*365 + yy/4 - yy/100 + yy/400
	cum := [...]int{0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334}
	days += cum[m-1] + d
	if m > 2 && (y%4 == 0 && (y%100 != 0 || y%400 == 0)) {
		days++
	}
	return days, true
}

func money(d decimal.Decimal) decimal.Decimal { return d.Round(2) }

const (
	pass   = 'P'
	fail   = 'F'
	unable = 'U'
	na     = 'N'
)

func verdict(f, u bool, ok byte) byte {
	if f {
		return fail
	}
	if u {
		return unable
	}
	return ok
}

func (p *Pack) known(code *string) bool {
	if empty(code) {
		return false
	}
	_, ok := p.Services[*code]
	return ok
}

// ---- the fifteen rules ----------------------------------------------------------------------------------------------

func (p *Pack) policy(c *Claim) *Policy {
	if c.PolicyID == nil {
		return nil
	}
	return p.Policies[*c.PolicyID]
}

func r001(c *Claim) byte {
	bad := empty(c.InvoiceNumber) || empty(c.MemberID) || empty(c.DiagnosisCode)
	for i := range c.Lines {
		l := &c.Lines[i]
		if empty(l.ServiceDate) || empty(l.ServiceCode) || !l.Quantity.ok || !l.UnitPrice.ok || !l.NetAmount.ok {
			bad = true
		}
	}
	if bad {
		return fail
	}
	return pass
}

func r002(c *Claim) byte {
	sub, subOK := day(c.SubmissionDate)
	var f, u bool
	for i := range c.Lines {
		d, ok := day(c.Lines[i].ServiceDate)
		if !ok || !subOK {
			u = true
		} else if d > sub {
			f = true
		}
	}
	return verdict(f, u, pass)
}

func r003(c *Claim) byte {
	cv := &c.Coverage
	start, sOK := day(cv.StartDate)
	end, eOK := day(cv.EndDate)
	var f, u bool
	if empty(cv.Status) {
		u = true
	} else if *cv.Status != "active" {
		f = true
	}
	if !sOK || !eOK {
		u = true
	}
	for i := range c.Lines {
		d, ok := day(c.Lines[i].ServiceDate)
		if !ok {
			u = true
		} else if (sOK && d < start) || (eOK && d > end) {
			f = true
		}
	}
	return verdict(f, u, pass)
}

func r004(c *Claim) byte {
	var f, u bool
	pairs := [2][2]*string{{c.PatientID, c.Coverage.Beneficiary}, {c.MemberID, c.Coverage.MemberID}}
	for _, pr := range pairs {
		if empty(pr[0]) || empty(pr[1]) {
			u = true
		} else if *pr[0] != *pr[1] {
			f = true
		}
	}
	return verdict(f, u, pass)
}

func (p *Pack) r005(c *Claim) byte {
	pol := p.policy(c)
	if empty(c.ProviderID) || pol == nil {
		return unable
	}
	if p.allowed[*c.PolicyID][*c.ProviderID] {
		return pass
	}
	return fail
}

func r006(c *Claim) byte {
	type key struct{ code, date, mod string }
	seen := make(map[key]struct{}, len(c.Lines))
	var dup, missing bool
	for i := range c.Lines {
		l := &c.Lines[i]
		if empty(l.ServiceCode) {
			missing = true
			continue
		}
		if _, ok := day(l.ServiceDate); !ok {
			missing = true
			continue
		}
		k := key{*l.ServiceCode, *l.ServiceDate, sval(l.Modifier)}
		if _, ok := seen[k]; ok {
			dup = true
		}
		seen[k] = struct{}{}
	}
	return verdict(dup, missing, pass)
}

func r007(c *Claim) byte {
	var f, u bool
	for i := range c.Lines {
		l := &c.Lines[i]
		if !l.Quantity.ok || !l.UnitPrice.ok || !l.NetAmount.ok {
			u = true
		} else if l.NetAmount.d.Sub(money(l.Quantity.d.Mul(l.UnitPrice.d))).Abs().GreaterThan(cent) {
			f = true
		}
	}
	return verdict(f, u, pass)
}

func (p *Pack) r008(c *Claim) byte {
	pol := p.policy(c)
	if pol == nil {
		return unable
	}
	var f, u, required bool
	req := p.authReq[*c.PolicyID]
	for i := range c.Lines {
		l := &c.Lines[i]
		if !p.known(l.ServiceCode) {
			u = true
		} else if req[*l.ServiceCode] {
			required = true
			if empty(l.AuthorizationID) {
				f = true
			}
		}
	}
	if required {
		return verdict(f, u, pass)
	}
	return verdict(f, u, na)
}

func (p *Pack) r009(c *Claim) byte {
	pol := p.policy(c)
	if pol == nil {
		return unable
	}
	var f, u, required bool
	req := p.authReq[*c.PolicyID]
	for i := range c.Lines {
		l := &c.Lines[i]
		if !p.known(l.ServiceCode) {
			u = true
			continue
		}
		if !req[*l.ServiceCode] {
			continue
		}
		required = true
		if empty(l.AuthorizationID) {
			u = true
			continue
		}
		var rec *Auth
		for j := range c.Authorizations {
			if c.Authorizations[j].AuthorizationID != nil && *c.Authorizations[j].AuthorizationID == *l.AuthorizationID {
				rec = &c.Authorizations[j]
				break
			}
		}
		if rec == nil {
			f = true
			continue
		}
		if empty(rec.PatientID) {
			u = true
		} else if c.PatientID == nil || *rec.PatientID != *c.PatientID {
			f = true
		}
		if empty(rec.ServiceCode) {
			u = true
		} else if *rec.ServiceCode != *l.ServiceCode {
			f = true
		}
		if empty(rec.Status) {
			u = true
		} else if *rec.Status != "approved" {
			f = true
		}
		d, dOK := day(l.ServiceDate)
		lo, loOK := day(rec.ValidFrom)
		hi, hiOK := day(rec.ValidTo)
		if !dOK || !loOK || !hiOK {
			u = true
		} else if !(lo <= d && d <= hi) {
			f = true
		}
	}
	// cumulative quantity per authorization
	done := map[string]bool{}
	for i := range c.Lines {
		l := &c.Lines[i]
		if empty(l.AuthorizationID) || !p.known(l.ServiceCode) || !req[*l.ServiceCode] || done[*l.AuthorizationID] {
			continue
		}
		done[*l.AuthorizationID] = true
		var rec *Auth
		for j := range c.Authorizations {
			if c.Authorizations[j].AuthorizationID != nil && *c.Authorizations[j].AuthorizationID == *l.AuthorizationID {
				rec = &c.Authorizations[j]
				break
			}
		}
		if rec == nil {
			continue
		}
		sum := decimal.Zero
		missing := !rec.MaxQuantity.ok
		for k := range c.Lines {
			m := &c.Lines[k]
			if m.AuthorizationID != nil && *m.AuthorizationID == *l.AuthorizationID {
				if !m.Quantity.ok {
					missing = true
				} else {
					sum = sum.Add(m.Quantity.d)
				}
			}
		}
		if missing {
			u = true
		} else if sum.GreaterThan(rec.MaxQuantity.d) {
			f = true
		}
	}
	if required {
		return verdict(f, u, pass)
	}
	return verdict(f, u, na)
}

func (p *Pack) r010(c *Claim) byte {
	pol := p.policy(c)
	if pol == nil {
		return unable
	}
	var f, u, required bool
	for i := range c.Lines {
		l := &c.Lines[i]
		if !p.known(l.ServiceCode) {
			u = true
			continue
		}
		need, ok := pol.RequiredDocuments[*l.ServiceCode]
		if !ok {
			continue
		}
		required = true
		d, dOK := day(l.ServiceDate)
		if !dOK {
			u = true
			continue
		}
		hits, final := 0, false
		for j := range c.Attachments {
			a := &c.Attachments[j]
			if a.Type == nil || *a.Type != need || a.PatientID == nil || c.PatientID == nil || *a.PatientID != *c.PatientID ||
				a.ServiceCode == nil || *a.ServiceCode != *l.ServiceCode {
				continue
			}
			ad, adOK := day(a.ServiceDate)
			if !adOK || ad != d {
				continue
			}
			hits++
			if a.DocumentStatus != nil && *a.DocumentStatus == "final" {
				final = true
			}
		}
		if hits == 0 {
			f = true
		} else if !final {
			u = true
		}
	}
	if required {
		return verdict(f, u, pass)
	}
	return verdict(f, u, na)
}

func (p *Pack) r011(c *Claim) byte {
	var f, u bool
	for i := range c.Lines {
		l := &c.Lines[i]
		if empty(l.ServiceCode) {
			u = true
		} else if _, ok := p.Services[*l.ServiceCode]; !ok {
			f = true
		}
	}
	return verdict(f, u, pass)
}

func r012(c *Claim) byte {
	if !c.TotalAmount.ok {
		return unable
	}
	sum := decimal.Zero
	for i := range c.Lines {
		if !c.Lines[i].NetAmount.ok {
			return unable
		}
		sum = sum.Add(c.Lines[i].NetAmount.d)
	}
	if c.TotalAmount.d.Sub(money(sum)).Abs().GreaterThan(cent) {
		return fail
	}
	return pass
}

func (p *Pack) r013(c *Claim) byte {
	pol := p.policy(c)
	var f, u bool
	for i := range c.Lines {
		l := &c.Lines[i]
		limits := false
		var maxQ, maxP Num
		if pol != nil && !empty(l.ServiceCode) {
			var ok1, ok2 bool
			maxP, ok1 = pol.MaxUnitPrice[*l.ServiceCode]
			maxQ, ok2 = pol.MaxQuantityPerLine[*l.ServiceCode]
			limits = ok1 && ok2
		}
		if !l.Quantity.ok || !l.UnitPrice.ok || !limits {
			u = true
		}
		if l.Quantity.ok {
			q := l.Quantity.d
			if !q.IsPositive() || !q.Equal(q.Truncate(0)) {
				f = true
			}
			if limits && q.GreaterThan(maxQ.d) {
				f = true
			}
		}
		if l.UnitPrice.ok {
			un := l.UnitPrice.d
			if !un.IsPositive() {
				f = true
			}
			if limits && un.GreaterThan(maxP.d) {
				f = true
			}
		}
	}
	return verdict(f, u, pass)
}

func (p *Pack) r014(c *Claim) byte {
	pol := p.policy(c)
	sub, subOK := day(c.SubmissionDate)
	if pol == nil || !subOK || len(c.Lines) == 0 {
		return unable
	}
	latest := 0
	for i := range c.Lines {
		d, ok := day(c.Lines[i].ServiceDate)
		if !ok {
			return unable
		}
		if d > latest {
			latest = d
		}
	}
	lag := sub - latest
	if lag < 0 {
		return na
	}
	if lag > pol.SubmissionWindowDays {
		return fail
	}
	return pass
}

func (p *Pack) r015(c *Claim) byte {
	pol := p.policy(c)
	if empty(c.Currency) || pol == nil {
		return unable
	}
	if *c.Currency == pol.Currency {
		return pass
	}
	return fail
}

// Evaluate writes the fifteen status codes (R001..R015) into out.
func (p *Pack) Evaluate(c *Claim, out *[15]byte) {
	out[0] = r001(c)
	out[1] = r002(c)
	out[2] = r003(c)
	out[3] = r004(c)
	out[4] = p.r005(c)
	out[5] = r006(c)
	out[6] = r007(c)
	out[7] = p.r008(c)
	out[8] = p.r009(c)
	out[9] = p.r010(c)
	out[10] = p.r011(c)
	out[11] = r012(c)
	out[12] = p.r013(c)
	out[13] = p.r014(c)
	out[14] = p.r015(c)
}
