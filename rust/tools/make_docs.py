"""Render synthetic claims as documents, with the answer key, to test the document reader and the document-to-claim conversion.

    python rust/tools/make_docs.py --out <dir> [--claims data/validation/claims.jsonl] [--n 30] [--seed 1]

Needs: pip install reportlab pypdfium2 pillow numpy
For each claim it writes, in two layouts (A "claim form", B "invoice") and four conditions:
    digital  a PDF with a text layer                      (what an EHR export looks like)
    scan     the page rendered to an image, lightly skewed, noisy, JPEG        (a flatbed scan)
    photo    the page rendered, rotated, blurred, uneven light, heavy JPEG     (a phone picture)
    scanpdf  an image-only PDF made from the scan          (a scanner's PDF: no text layer)
plus `truth.json`: {file name: the claim fields the page shows}. Nothing here is real data: the claims are the synthetic pack's.
The page shows: claim id, invoice number, patient, member, provider, payer, policy, diagnosis, submission date, currency, total, the coverage
status and dates, and every line (id, service code, date, modifier, quantity, unit price, net amount, authorization).
"""
import argparse
import io
import json
import random
from pathlib import Path

import numpy as np
import pypdfium2 as pdfium
from PIL import Image, ImageEnhance, ImageFilter
from reportlab.lib import colors
from reportlab.lib.pagesizes import A4
from reportlab.lib.styles import ParagraphStyle, getSampleStyleSheet
from reportlab.lib.units import mm
from reportlab.platypus import Paragraph, SimpleDocTemplate, Spacer, Table, TableStyle

SCALAR_FIELDS = ['claim_id', 'invoice_number', 'patient_id', 'member_id', 'provider_id', 'payer_id', 'policy_id', 'diagnosis_code', 'submission_date', 'currency', 'total_amount']


def money(v):
    return '' if v is None else ('%.2f' % v if not float(v).is_integer() else '%d.00' % int(v))


def shown(claim):
    """The part of a claim the page shows (and so the key a reader is scored against)."""
    cov = claim.get('coverage') or {}
    return {
        **{k: claim.get(k) for k in SCALAR_FIELDS},
        'coverage': {'status': cov.get('status'), 'start_date': cov.get('start_date'), 'end_date': cov.get('end_date')},
        'lines': [{k: l.get(k) for k in ('line_id', 'service_code', 'service_date', 'modifier', 'quantity', 'unit_price', 'net_amount', 'authorization_id')} for l in claim['lines']],
    }


def layout_a(claim, path):
    ss = getSampleStyleSheet()
    body = ParagraphStyle('b', parent=ss['Normal'], fontName='Helvetica', fontSize=10, leading=14)
    h = ParagraphStyle('h', parent=ss['Title'], fontName='Helvetica-Bold', fontSize=18)
    cov = claim['coverage'] or {}
    doc = SimpleDocTemplate(str(path), pagesize=A4, leftMargin=18 * mm, rightMargin=18 * mm, topMargin=16 * mm, bottomMargin=16 * mm)
    rows = [['Claim ID:', claim['claim_id'], 'Submitted:', claim['submission_date']],
            ['Invoice No:', claim['invoice_number'] or '', 'Currency:', claim['currency']],
            ['Patient ID:', claim['patient_id'], 'Member ID:', claim['member_id'] or ''],
            ['Provider:', claim['provider_id'], 'Payer:', claim['payer_id']],
            ['Policy:', claim['policy_id'], 'Diagnosis:', claim['diagnosis_code'] or '']]
    t1 = Table(rows, colWidths=[28 * mm, 58 * mm, 28 * mm, 58 * mm])
    t1.setStyle(TableStyle([('FONTNAME', (0, 0), (-1, -1), 'Helvetica'), ('FONTNAME', (0, 0), (0, -1), 'Helvetica-Bold'), ('FONTNAME', (2, 0), (2, -1), 'Helvetica-Bold'), ('FONTSIZE', (0, 0), (-1, -1), 10)]))
    head = ['Line', 'Service', 'Date', 'Mod', 'Qty', 'Unit price', 'Amount', 'Auth']
    data = [head] + [[l['line_id'], l['service_code'] or '', l['service_date'] or '', l['modifier'] or '-', ('%g' % l['quantity']) if l['quantity'] is not None else '', money(l['unit_price']), money(l['net_amount']), l['authorization_id'] or '-'] for l in claim['lines']]
    t2 = Table(data, repeatRows=1, colWidths=[12 * mm, 30 * mm, 24 * mm, 14 * mm, 12 * mm, 22 * mm, 22 * mm, 42 * mm])
    t2.setStyle(TableStyle([('GRID', (0, 0), (-1, -1), 0.4, colors.grey), ('BACKGROUND', (0, 0), (-1, 0), colors.lightgrey), ('FONTNAME', (0, 0), (-1, 0), 'Helvetica-Bold'), ('FONTNAME', (0, 1), (-1, -1), 'Helvetica'), ('FONTSIZE', (0, 0), (-1, -1), 8.5)]))
    story = [Paragraph('CLAIM FORM', h), Spacer(1, 4 * mm), t1, Spacer(1, 3 * mm),
             Paragraph('Coverage: %s, %s to %s' % (cov.get('status') or '', cov.get('start_date') or '', cov.get('end_date') or ''), body), Spacer(1, 5 * mm),
             Paragraph('<b>SERVICES</b>', body), Spacer(1, 2 * mm), t2, Spacer(1, 5 * mm),
             Paragraph('Total amount: %s %s' % (money(claim['total_amount']), claim['currency']), body)]
    doc.build(story)


def layout_b(claim, path):
    ss = getSampleStyleSheet()
    body = ParagraphStyle('b', parent=ss['Normal'], fontName='Helvetica', fontSize=10.5, leading=15)
    h = ParagraphStyle('h', parent=ss['Heading1'], fontName='Helvetica-Bold', fontSize=16)
    cov = claim['coverage'] or {}
    doc = SimpleDocTemplate(str(path), pagesize=A4, leftMargin=20 * mm, rightMargin=20 * mm, topMargin=18 * mm, bottomMargin=16 * mm)
    story = [Paragraph('INVOICE %s' % (claim['invoice_number'] or ''), h),
             Paragraph('Invoice date: %s' % claim['submission_date'], body),
             Paragraph('Reference (claim): %s' % claim['claim_id'], body), Spacer(1, 3 * mm),
             Paragraph('Patient: %s' % claim['patient_id'], body),
             Paragraph('Member No.: %s' % (claim['member_id'] or ''), body),
             Paragraph('Treating provider: %s' % claim['provider_id'], body),
             Paragraph('Insurer: %s / Plan: %s' % (claim['payer_id'], claim['policy_id']), body),
             Paragraph('ICD / Dx code: %s' % (claim['diagnosis_code'] or ''), body),
             Paragraph('Coverage period: %s - %s (%s)' % (cov.get('start_date') or '', cov.get('end_date') or '', cov.get('status') or ''), body), Spacer(1, 5 * mm)]
    head = ['#', 'Code', 'Service date', 'Mod.', 'Qty', 'Price', 'Net', 'Prior auth.']
    data = [head] + [[l['line_id'], l['service_code'] or '', l['service_date'] or '', l['modifier'] or '-', ('%g' % l['quantity']) if l['quantity'] is not None else '', money(l['unit_price']), money(l['net_amount']), l['authorization_id'] or '-'] for l in claim['lines']]
    t = Table(data, repeatRows=1, colWidths=[10 * mm, 32 * mm, 26 * mm, 14 * mm, 12 * mm, 20 * mm, 20 * mm, 42 * mm])
    t.setStyle(TableStyle([('LINEBELOW', (0, 0), (-1, 0), 0.8, colors.black), ('FONTNAME', (0, 0), (-1, 0), 'Helvetica-Bold'), ('FONTNAME', (0, 1), (-1, -1), 'Helvetica'), ('FONTSIZE', (0, 0), (-1, -1), 9), ('ROWBACKGROUNDS', (0, 1), (-1, -1), [colors.white, colors.whitesmoke])]))
    story += [t, Spacer(1, 6 * mm), Paragraph('<b>Amount payable: %s %s</b>' % (claim['currency'], money(claim['total_amount'])), body)]
    doc.build(story)


def render(pdf_path, dpi=170):
    pdf = pdfium.PdfDocument(str(pdf_path))
    img = pdf[0].render(scale=dpi / 72).to_pil().convert('RGB')
    pdf.close()
    return img


def degrade(img, rng, level):
    img = img.copy()
    if level == 'scan':
        img = img.rotate(rng.uniform(-1.5, 1.5), resample=Image.BICUBIC, expand=True, fillcolor=(250, 250, 250))
        arr = np.asarray(img).astype(np.float32)
        arr += np.random.default_rng(rng.randrange(1 << 30)).normal(0, 6, arr.shape)
        img = Image.fromarray(np.clip(arr, 0, 255).astype(np.uint8))
        q = 70
    else:
        img = img.rotate(rng.uniform(-3.5, 3.5), resample=Image.BICUBIC, expand=True, fillcolor=(235, 232, 225))
        img = img.filter(ImageFilter.GaussianBlur(rng.uniform(0.9, 1.5)))
        w, h = img.size
        grad = np.linspace(rng.uniform(0.72, 0.9), 1.0, w)[None, :, None]       # uneven light across the page
        arr = np.asarray(img).astype(np.float32) * grad
        arr += np.random.default_rng(rng.randrange(1 << 30)).normal(0, 9, arr.shape)
        img = Image.fromarray(np.clip(arr, 0, 255).astype(np.uint8))
        img = ImageEnhance.Contrast(img).enhance(rng.uniform(0.8, 0.95))
        img = img.resize((int(w * 0.62), int(h * 0.62)), Image.LANCZOS)          # a phone picture is smaller than a scan
        q = 35
    buf = io.BytesIO()
    img.save(buf, 'JPEG', quality=q)
    return Image.open(io.BytesIO(buf.getvalue())).convert('RGB'), q


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--out', required=True)
    ap.add_argument('--claims', default='data/validation/claims.jsonl')
    ap.add_argument('--n', type=int, default=30)
    ap.add_argument('--seed', type=int, default=1)
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    claims = [json.loads(l) for l in Path(a.claims).read_text(encoding='utf-8').split(chr(10)) if l.strip()]
    rng = random.Random(a.seed)
    rng.shuffle(claims)
    claims = [c for c in claims if c.get('coverage') and 1 <= len(c['lines']) <= 6][:a.n]
    truth = {}
    for c in claims:
        key = shown(c)
        for lay, fn in (('A', layout_a), ('B', layout_b)):
            base = '%s_%s' % (c['claim_id'], lay)
            pdf = out / (base + '_digital.pdf')
            fn(c, pdf)
            truth[pdf.name] = key
            page = render(pdf)
            for cond in ('scan', 'photo'):
                img, q = degrade(page, rng, cond)
                p = out / ('%s_%s.%s' % (base, cond, 'png' if cond == 'scan' else 'jpg'))
                if cond == 'scan':
                    img.save(p, 'PNG')
                else:
                    img.save(p, 'JPEG', quality=q)
                truth[p.name] = key
                if cond == 'scan':
                    sp = out / (base + '_scanpdf.pdf')
                    img.save(sp, 'PDF', resolution=150)
                    truth[sp.name] = key
    (out / 'truth.json').write_text(json.dumps(truth, indent=1), encoding='utf-8')
    print('wrote %d documents for %d claims in %s' % (len(truth), len(claims), out))


if __name__ == '__main__':
    main()
