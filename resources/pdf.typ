#import "cmarker/lib.typ": render
#let mm-input(name, default) = float(sys.inputs.at(name, default: default).replace("mm", "")) * 1mm
#set page(width: mm-input("pageWidth", "210mm"), height: mm-input("pageHeight", "297mm"), margin: mm-input("margin", "20mm"))
#set text(font: sys.inputs.at("font", default: "Noto Sans CJK SC"), size: 11pt)
#set heading(numbering: none)
#render(read(sys.inputs.at("source")), raw-typst: false)
