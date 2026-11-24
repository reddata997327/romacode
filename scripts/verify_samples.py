"""Verify the generated samples with independent readers (python + soffice)."""
import glob, os, sys, zipfile, subprocess

S = "/home/z/my-project/work/samples"
ok, fail = 0, 0
def check(name, cond, detail=""):
    global ok, fail
    print(("PASS " if cond else "FAIL ") + name + ((" — " + detail) if detail else ""))
    ok += cond; fail += (not cond)

# zip-level sanity (OOXML)
for f, part in [("RomaCode_Documents_Report.docx", "word/document.xml"),
                ("RomaCode_Sales.xlsx", "xl/workbook.xml"),
                ("RomaCode_Deck.pptx", "ppt/presentation.xml")]:
    p = os.path.join(S, f)
    check(f + " exists", os.path.exists(p) and os.path.getsize(p) > 1000)
    if os.path.exists(p):
        z = zipfile.ZipFile(p)
        check(f + " has " + part, part in z.namelist())
        bad = z.testzip()
        check(f + " zip integrity", bad is None)

# PDF header + EOF
p = os.path.join(S, "RomaCode_Overview.pdf")
data = open(p, "rb").read()
check("pdf header", data.startswith(b"%PDF-1.7"))
check("pdf EOF", b"%%EOF" in data[-32:])
check("pdf pages", data.count(b"/Type /Page ") == 3)

# PNGs non-trivial
for f in ["revenue.png", "poster.png"]:
    p = os.path.join(S, f)
    check(f, os.path.exists(p) and os.path.getsize(p) > 10000)

# soffice render (real-world open test) for docx/xlsx/pptx/pdf
os.makedirs("/tmp/render2", exist_ok=True)
for f in ["RomaCode_Documents_Report.docx", "RomaCode_Sales.xlsx", "RomaCode_Deck.pptx", "RomaCode_Overview.pdf"]:
    src = os.path.join(S, f)
    if not os.path.exists(src):
        check("soffice " + f, False, "missing"); continue
    r = subprocess.run(["soffice", "--headless", "--convert-to", "pdf", "--outdir", "/tmp/render2", src],
                       capture_output=True, timeout=180)
    out = "/tmp/render2/" + os.path.splitext(f)[0] + ".pdf"
    check("soffice opens " + f, os.path.exists(out) and os.path.getsize(out) > 2000,
          (r.stderr.decode()[:120] if not os.path.exists(out) else ""))

print(f"\n{ok} passed, {fail} failed")
sys.exit(1 if fail else 0)
