"""Regenerate the tiny deterministic document fixtures; no runtime dependency."""
from pathlib import Path
import zipfile

ROOT = Path(__file__).parent
NS_REL = 'http://schemas.openxmlformats.org/package/2006/relationships'
OFFICE_REL = 'http://schemas.openxmlformats.org/officeDocument/2006/relationships'

def package(name, parts, root):
    content = ['<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>']
    types = {'word/document.xml': 'wordprocessingml.document.main+xml', 'xl/workbook.xml': 'spreadsheetml.sheet.main+xml', 'xl/worksheets/sheet1.xml': 'spreadsheetml.worksheet+xml', 'ppt/presentation.xml': 'presentationml.presentation.main+xml', 'ppt/slides/slide1.xml': 'presentationml.slide+xml', 'ppt/slides/slide2.xml': 'presentationml.slide+xml'}
    for part in parts:
        if part in types: content.append(f'<Override PartName="/{part}" ContentType="application/vnd.openxmlformats-officedocument.{types[part]}"/>')
    content.append('</Types>')
    parts = {'[Content_Types].xml': ''.join(content), '_rels/.rels': f'<Relationships xmlns="{NS_REL}"><Relationship Id="rId1" Type="{OFFICE_REL}/officeDocument" Target="{root}"/></Relationships>', **parts}
    with zipfile.ZipFile(ROOT / name, 'w') as archive:
        for path, value in sorted(parts.items()):
            item = zipfile.ZipInfo(path, date_time=(1980, 1, 1, 0, 0, 0))
            item.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(item, value.encode())

package('handbook.docx', {'word/document.xml': '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>面试手册 &amp; 评估标准</w:t></w:r></w:p><w:p><w:r><w:t>Assess Rust and distributed systems experience.</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Architecture: 5 points</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>'}, 'word/document.xml')
package('scores.xlsx', {'xl/workbook.xml': f'<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{OFFICE_REL}"><sheets><sheet name="面试分数" sheetId="1" r:id="rId1"/></sheets></workbook>', 'xl/_rels/workbook.xml.rels': f'<Relationships xmlns="{NS_REL}"><Relationship Id="rId1" Type="{OFFICE_REL}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>', 'xl/worksheets/sheet1.xml': '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:B1000000"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Rust</t></is></c><c r="B1"><v>5</v></c></row><row r="1000000"><c r="B1000000"><f>SUM(B1:B2)</f><v>8</v></c></row></sheetData></worksheet>'}, 'xl/workbook.xml')
package('slides.pptx', {'ppt/presentation.xml': f'<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="{OFFICE_REL}"><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId1"/></p:sldIdLst></p:presentation>', 'ppt/_rels/presentation.xml.rels': f'<Relationships xmlns="{NS_REL}"><Relationship Id="rId1" Type="{OFFICE_REL}/slide" Target="slides/slide1.xml"/><Relationship Id="rId2" Type="{OFFICE_REL}/slide" Target="slides/slide2.xml"/></Relationships>', **{f'ppt/slides/slide{i}.xml': f'<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>' for i, text in [(1, 'Second: Rust skills'), (2, 'First: Candidate review')]}}, 'ppt/presentation.xml')

def pdf(name, pages):
    objects = [b'<< /Type /Catalog /Pages 2 0 R >>', f'<< /Type /Pages /Kids [{" ".join(f"{4+i*2} 0 R" for i in range(len(pages)))}] /Count {len(pages)} >>'.encode(), b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>']
    for i, text in enumerate(pages):
        objects.append(f'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {5+i*2} 0 R >>'.encode())
        data = f'BT /F1 12 Tf 72 720 Td ({text}) Tj ET'.encode() if text else b''
        objects.append(f'<< /Length {len(data)} >>\nstream\n'.encode() + data + b'\nendstream')
    result = bytearray(b'%PDF-1.4\n'); offsets = [0]
    for i, obj in enumerate(objects, 1):
        offsets.append(len(result)); result.extend(f'{i} 0 obj\n'.encode() + obj + b'\nendobj\n')
    start = len(result)
    result.extend(f'xref\n0 {len(objects)+1}\n0000000000 65535 f \n'.encode())
    for offset in offsets[1:]: result.extend(f'{offset:010d} 00000 n \n'.encode())
    result.extend(f'trailer\n<< /Size {len(objects)+1} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n'.encode())
    (ROOT/name).write_bytes(result)

pdf('resume.pdf', ['Candidate: experienced Rust engineer', 'Distributed systems: five years'])
pdf('scan.pdf', [''])
