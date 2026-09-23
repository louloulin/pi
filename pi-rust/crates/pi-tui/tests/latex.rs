//! Port of `packages/tui/test/latex.test.ts`.
//!
//! Every case mirrors an assertion from the upstream Node test so the Rust
//! renderer can be diffed against the TypeScript oracle case by case.

use pi_tui::latex::{render_latex, render_latex_with};

fn check(source: &str, expected: &str) {
    assert_eq!(
        render_latex(source).as_deref(),
        Some(expected),
        "render_latex({source:?})"
    );
}

fn check_display(source: &str, expected: &str) {
    assert_eq!(
        render_latex_with(source, true).as_deref(),
        Some(expected),
        "render_latex_with({source:?}, true)"
    );
}

#[test]
fn jacobian_conjecture_dollar_delimited() {
    check(r"\mathbb{C}^3 \to \mathbb{C}^3", "ℂ³ → ℂ³");
    check(
        r"\{3x+2y,\; 27x^2-4z-1,\; x(x-1)(x+1)\} \quad\Rightarrow\quad x \in \{0, \pm 1\},",
        "{3x+2y, 27x²-4z-1, x(x-1)(x+1)} ⇒ x ∈ {0, ± 1},",
    );
    check(r"F_1 = -\frac{1}{4x^2}.", "F₁ = -1/(4x²).");
    check("-2", "-2");
    check("(0,0,-1/4)", "(0,0,-1/4)");
    check("(1,-3/2,13/2)", "(1,-3/2,13/2)");
    check("(1,1,1)", "(1,1,1)");
    check("(2,1,0)", "(2,1,0)");
    check("(-1/4, 0, 0)", "(-1/4, 0, 0)");
    check(
        r"\{(0,0,-1/4), (1,-3/2,13/2), (-1,3/2,13/2)\}",
        "{(0,0,-1/4), (1,-3/2,13/2), (-1,3/2,13/2)}",
    );
    check("(2,1,1)", "(2,1,1)");
    check("(7/3,-2/5,11/7)", "(7/3,-2/5,11/7)");
    check(r"\{y - p(x),\; q(x)\}", "{y - p(x), q(x)}");
    check(r"\deg q = 3", "deg q = 3");
    check(
        r"[\mathbb{C}(x,y,z):\mathbb{C}(F_1,F_2,F_3)] = 3",
        "[ℂ(x,y,z):ℂ(F₁,F₂,F₃)] = 3",
    );
    check("u = 1+xy", "u = 1+xy");
    check("G = u^2 z + y^2(4+3xy)", "G = u² z + y²(4+3xy)");
    check("F_1 = uG", "F₁ = uG");
    check("F_2 = y + 3xG", "F₂ = y + 3xG");
    check("x=0", "x = 0");
    check("F_2 = F_3 = 0", "F₂ = F₃ = 0");
    check("xy = -3/2", "xy = -3/2");
    check("x^2 z = 13/2", "x² z = 13/2");
    check(r"\mathbb{C}^*", "ℂ^*");
    check(
        r"s \mapsto (s,\, -\tfrac{3}{2s},\, \tfrac{13}{2s^2})",
        "s ↦ (s, -3/(2s), 13/(2s²))",
    );
    check("X", "X");
    check(r"p_\pm", "p_±");
    check(
        "F(-x,-y,z) = (F_1, -F_2, -F_3)",
        "F(-x,-y,z) = (F₁, -F₂, -F₃)",
    );
    check("p_0", "p₀");
    check(r"s \to \infty", "s → ∞");
    check("(0,0,0)", "(0,0,0)");
    check(r"\Rightarrow", "⇒");
    check(r"\ge 2", "≥ 2");
    check(r"\ge 3", "≥ 3");
    check("1", "1");
    check(r"\mathrm{diag}(-1/2,1,1)", "diag(-1/2,1,1)");
    check("4+3xy", "4+3xy");
}

#[test]
fn satellite_calculation_bracket_delimited() {
    check(
        r"E \approx \frac{0.1\ \text{lux}}{100\ \text{lm/W}} = 0.001\ \text{W/m}^2",
        "E ≈ (0.1 lux)/(100 lm/W) = 0.001 W/m²",
    );
    check(
        r"\boxed{1\ \text{milliwatt per square metre}}",
        "[1 milliwatt per square metre]",
    );
    check(
        r"5\ \text{km}^2 = 5{,}000{,}000\ \text{m}^2",
        "5 km² = 5,000,000 m²",
    );
    check(
        "P_{\\text{light}} = 0.001 \\times 5{,}000{,}000\n= \\boxed{5{,}000\\ \\text{W}}",
        "P_light = 0.001 × 5,000,000 = [5,000 W]",
    );
    check(
        "P_{\\text{electric}} = 5\\ \\text{kW} \\times 0.2\n= \\boxed{1\\ \\text{kW}}",
        "P_electric = 5 kW × 0.2 = [1 kW]",
    );
    check(
        r"\pi(2.5\ \text{km})^2 = 19.6\ \text{km}^2",
        "π(2.5 km)² = 19.6 km²",
    );
    check(
        "0.001\\ \\text{W/m}^2 \\times 19.6 \\times 10^6\\ \\text{m}^2\n\\approx \\boxed{20\\ \\text{kW optical}}",
        "0.001 W/m² × 19.6 × 10⁶ m² ≈ [20 kW optical]",
    );
    check(
        "1\\ \\text{kW} \\times \\frac{1}{3600}\\ \\text{hour}\n= \\boxed{0.28\\ \\text{Wh}}",
        "1 kW × 1/3600 hour = [0.28 Wh]",
    );
}

#[test]
fn jacobian_conjecture_parenthesis_and_bracket_delimited() {
    check(
        r"\det\!\left(\frac{\partial(F_1,F_2,F_3)}{\partial(x,y,z)}\right)=-2.",
        "det((∂(F₁,F₂,F₃))/(∂(x,y,z))) = -2.",
    );
    check(
        "\\begin{aligned}\nF(0,0,-\\tfrac14)&=(-\\tfrac14,0,0),\\\\\nF(1,-\\tfrac32,\\tfrac{13}2)&=(-\\tfrac14,0,0),\\\\\nF(-1,\\tfrac32,\\tfrac{13}2)&=(-\\tfrac14,0,0).\n\\end{aligned}",
        "F(0,0,-1/4) = (-1/4,0,0),\nF(1,-3/2,13/2) = (-1/4,0,0),\nF(-1,3/2,13/2) = (-1/4,0,0).",
    );
    check("F=(F_1,F_2,F_3)", "F = (F₁,F₂,F₃)");
    check("F", "F");
    check("3", "3");
}

#[test]
fn jacobian_matrix_session() {
    check(
        "J = \\begin{pmatrix}\n\\frac{\\partial f_1}{\\partial x} & \\frac{\\partial f_1}{\\partial y} & \\frac{\\partial f_1}{\\partial z} \\\\\n\\frac{\\partial f_2}{\\partial x} & \\frac{\\partial f_2}{\\partial y} & \\frac{\\partial f_2}{\\partial z} \\\\\n\\frac{\\partial f_3}{\\partial x} & \\frac{\\partial f_3}{\\partial y} & \\frac{\\partial f_3}{\\partial z}\n\\end{pmatrix}",
        "J = ⎛ (∂ f₁)/(∂ x) │ (∂ f₁)/(∂ y) │ (∂ f₁)/(∂ z) ⎞\n    ⎜ (∂ f₂)/(∂ x) │ (∂ f₂)/(∂ y) │ (∂ f₂)/(∂ z) ⎟\n    ⎝ (∂ f₃)/(∂ x) │ (∂ f₃)/(∂ y) │ (∂ f₃)/(∂ z) ⎠",
    );
    check(
        "\\begin{aligned}\nf_1 &= (1+xy)^3 z + y^2(1+xy)(4+3xy) \\\\\nf_2 &= y + 3x(1+xy)^2 z + 3xy^2(4+3xy) \\\\\nf_3 &= 2x - 3x^2y - x^3z\n\\end{aligned}",
        "f₁ = (1+xy)³ z + y²(1+xy)(4+3xy)\nf₂ = y + 3x(1+xy)² z + 3xy²(4+3xy)\nf₃ = 2x - 3x²y - x³z",
    );
    check("x, y, z", "x, y, z");
    check("(x, y, z)", "(x, y, z)");
    check(r"(0,\; 0,\; -\tfrac14)", "(0, 0, -1/4)");
    check(r"(-\tfrac14,\; 0,\; 0)", "(-1/4, 0, 0)");
    check(r"(1,\; -\tfrac32,\; \tfrac{13}{2})", "(1, -3/2, 13/2)");
    check(r"(-1,\; \tfrac32,\; \tfrac{13}{2})", "(-1, 3/2, 13/2)");
    check(r"(-\frac14, 0, 0)", "(-1/4, 0, 0)");
    check(r"F: \mathbb{C}^3 \to \mathbb{C}^3", "F: ℂ³ → ℂ³");
    check(
        r"F(0,0,-\tfrac14) = F(1,-\tfrac32,\tfrac{13}{2}) = F(-1,\tfrac32,\tfrac{13}{2}) = (-\tfrac14, 0, 0)",
        "F(0,0,-1/4) = F(1,-3/2,13/2) = F(-1,3/2,13/2) = (-1/4, 0, 0)",
    );
    check(r"\mathbb{C}^3", "ℂ³");
    check(
        "\\begin{aligned}\nf_1 &= \\frac{f_1^{\\text{ut}}(u,t)}{x^2}, \\quad\nf_2 = \\frac{f_2^{\\text{ut}}(u,t)}{x}, \\quad\nf_3 = x\\,(2 - 3u - t)\n\\end{aligned}",
        "f₁ = (f₁ᵘᵗ(u,t))/(x²), f₂ = (f₂ᵘᵗ(u,t))/x, f₃ = x (2 - 3u - t)",
    );
    check(r"\det J_F", "det J_F");
    check(r"(-\tfrac14, 0, 0)", "(-1/4, 0, 0)");
    check("u = xy", "u = xy");
    check("t = x^2z", "t = x²z");
    check(r"x \neq 0", "x ≠ 0");
    check(r"f_1^{\text{ut}}, f_2^{\text{ut}}", "f₁ᵘᵗ, f₂ᵘᵗ");
    check("u,t", "u,t");
    check("x", "x");
    check("x, x^2", "x, x²");
    check(r"\mathbb{C}^n \to \mathbb{C}^n", "ℂⁿ → ℂⁿ");
    check(r"n \geq 2", "n ≥ 2");
    check(r"\mathbb{P}^3", "ℙ³");
}

#[test]
fn renderer_stress_test_formulas() {
    check(r"e^{i\pi}+1=0", "e^(iπ)+1 = 0");
    check(
        "\\boxed{\n\\mathcal{Z}(\\beta)\n=\n\\int_{\\mathcal M}\n\\exp\\!\\left(\n-\\beta\\left[\n\\frac12 g^{ij}(x)\\,\\partial_i\\phi\\,\\partial_j\\phi\n+V(\\phi)\n\\right]\\right)\n\\mathcal D\\phi\n}",
        "[Z(β) = ∫_M exp( -β[ 1/2 gⁱʲ(x) ∂ᵢϕ ∂ⱼϕ +V(ϕ) ]) Dϕ]",
    );
    check(
        "\\begin{aligned}\n\\nabla_\\mu T^{\\mu\\nu}\n&=\n\\frac{1}{\\sqrt{-g}}\n\\partial_\\mu\\!\\left(\\sqrt{-g}\\,T^{\\mu\\nu}\\right)\n+\\Gamma^\\nu_{\\mu\\lambda}T^{\\mu\\lambda}\n=0, \\\\[4pt]\nR_{\\mu\\nu}-\\frac12 Rg_{\\mu\\nu}+\\Lambda g_{\\mu\\nu}\n&=\n\\frac{8\\pi G}{c^4}T_{\\mu\\nu}.\n\\end{aligned}",
        "∇_μ T^(μν) = 1/(√(-g)) ∂_μ(√(-g) T^(μν)) +Γ^ν_(μλ)T^(μλ) = 0,\nR_(μν)-1/2 Rg_(μν)+Λ g_(μν) = (8π G)/(c⁴)T_(μν).",
    );
    let matrix_expected = [
        "f(z) = 1/(2π i) ∮_γ (f(ζ))/(ζ-z) dζ, det⎛ λ-a │ -b  │ 0   ⎞ = 0.".to_string(),
        format!("{}⎜ -c  │ λ-d │ -e  ⎟", " ".repeat(40)),
        format!("{}⎝ 0   │ -f  │ λ-g ⎠", " ".repeat(40)),
    ]
    .join("\n");
    check(
        "f(z)\n=\n\\frac{1}{2\\pi i}\n\\oint_{\\gamma}\n\\frac{f(\\zeta)}{\\zeta-z}\\,d\\zeta,\n\\qquad\n\\det\\!\\begin{pmatrix}\n\\lambda-a & -b & 0\\\\\n-c & \\lambda-d & -e\\\\\n0 & -f & \\lambda-g\n\\end{pmatrix}\n=0.",
        &matrix_expected,
    );
    check(
        "\\Psi(x,t)=\n\\sum_{n=1}^{\\infty}\n\\underbrace{\nc_n\n\\sqrt{\\frac{2}{L}}\n\\sin\\!\\left(\\frac{n\\pi x}{L}\\right)\n}_{\\text{spatial eigenmode}}\n\\exp\\!\\left(-\\frac{i\\hbar n^2\\pi^2}{2mL^2}t\\right),\n\\qquad\n|\\Psi(x,t)|^2\n=\n\\begin{cases}\n\\Psi^\\ast\\Psi, & 0<x<L,\\\\\n0, & \\text{otherwise}.\n\\end{cases}",
        "Ψ(x,t) = ∑ₙ₌₁^∞ cₙ √(2/L) sin((nπ x)/L)_(spatial eigenmode) exp(-(iℏ n²π²)/(2mL²)t), |Ψ(x,t)|² = ⎧ Ψ^∗Ψ if 0 < x < L,\n⎩ 0 otherwise.",
    );
    check(
        r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}",
        "x = (-b±√(b²-4ac))/(2a)",
    );
    check(
        r"\int_0^\infty e^{-x^2}\,dx=\frac{\sqrt{\pi}}{2}",
        "∫₀^∞ e^(-x²) dx = (√π)/2",
    );
    check(
        r"e^{i\theta}=\cos\theta+i\sin\theta",
        "e^(iθ) = cos θ+i sin θ",
    );
    check(
        r"\sum_{n=1}^{\infty}\frac{1}{n^2}=\frac{\pi^2}{6}",
        "∑ₙ₌₁^∞1/(n²) = π²/6",
    );
    check(r"\lim_{x\to 0}\frac{\sin x}{x}=1", "lim[x→0] (sin x)/x = 1");
    check(
        "\\lim_{n\\to\\infty}\n\\left(1+\\frac{1}{n}\\right)^n=e",
        "lim[n→∞] (1+1/n)ⁿ = e",
    );
    check(
        "\\int_0^1 \\frac{x^2}{1+x^3}\\,dx\n=\\frac{1}{3}\\ln 2",
        "∫₀¹ x²/(1+x³) dx = 1/3 ln 2",
    );
    check(
        "\\sum_{k=1}^{n}\\frac{k}{k+1}\n=n+1-H_{n+1}",
        "∑ₖ₌₁ⁿk/(k+1) = n+1-Hₙ₊₁",
    );
    check(
        "\\frac{\n  \\displaystyle \\frac{x^2+1}{x-1}\n  -\n  \\displaystyle \\frac{2x}{x+1}\n}{\n  \\displaystyle \\frac{x}{x^2-1}\n}",
        "((x²+1)/(x-1) - 2x/(x+1))/(x/(x²-1))",
    );
    check(
        "\\lim_{x\\to 0}\n\\frac{\n  \\displaystyle \\frac{\\sin x}{x}-1\n}{\n  \\displaystyle \\frac{e^x-1}{x}-1\n}\n=0",
        "lim[x→0] ((sin x)/x-1)/((eˣ-1)/x-1) = 0",
    );
    check(
        "\\frac{\n  1+\\displaystyle\\frac{1}{1+\\frac{1}{x}}\n}{\n  1-\\displaystyle\\frac{1}{1-\\frac{1}{x}}\n}",
        "(1+1/(1+1/x))/(1-1/(1-1/x))",
    );
    check(
        "\\sum_{n=1}^{\\infty}\n\\frac{\n  \\displaystyle \\frac{1}{n}-\\frac{1}{n+1}\n}{\n  \\displaystyle 1+\\frac{1}{n^2}\n}",
        "∑ₙ₌₁^∞ (1/n-1/(n+1))/(1+1/(n²))",
    );
}

#[test]
fn renders_common_symbols_roots_sums_and_integrals() {
    check(
        r"\sum_{i=0}^n \alpha_i + \int_0^\infty e^{-x^2}\,dx = \sqrt{\pi}",
        "∑ᵢ₌₀ⁿ αᵢ + ∫₀^∞ e^(-x²) dx = √π",
    );
}

#[test]
fn renders_common_accents_and_binomial_notation() {
    check(
        r"\binom{n}{k}+\vec{x}+\hat{y}+\overline{AB}",
        "(n choose k)+x⃗+ŷ+overline(AB)",
    );
}

#[test]
fn renders_extended_symbols_and_negated_relations() {
    check(
        r"\epsilon+\varepsilon+\varsigma+\varkappa+\oplus+\otimes+\therefore+\because",
        "ϵ+ε+ς+ϰ+⊕+⊗+∴+∵",
    );
    check(r"A\not\subseteq B,\quad x\not\in X", "A ⊈ B, x ∉ X");
}

#[test]
fn renders_relational_algebra_join_operators() {
    check(r"R\bowtie S,\quad R\Join S", "R ⋈ S, R ⋈ S");
    check(r"R\ltimes S,\quad R\rtimes S", "R ⋉ S, R ⋊ S");
    check(
        r"R\leftouterjoin S,\quad R\rightouterjoin S,\quad R\fullouterjoin S",
        "R ⟕ S, R ⟖ S, R ⟗ S",
    );
}

#[test]
fn renders_delimiter_commands_and_invisible_delimiters() {
    check(
        r"\lvert{x}\rvert+\lVert{v}\rVert+\left.\frac{dy}{dx}\right|_{x=0}",
        "|x|+‖v‖+dy/(dx)|ₓ₌₀",
    );
    check(
        r"\left\lbrace x \middle| x>0 \right\rbrace",
        "{ x | x > 0 }",
    );
}

#[test]
fn renders_named_modular_overlaid_and_underlaid_operators() {
    check(
        r"\operatorname*{arg\,max}_{x\in X} f(x)",
        "arg max[x∈X] f(x)",
    );
    check(r"a\bmod n,\quad a\equiv b\pmod n", "a mod n, a ≡ b (mod n)");
    check(
        r"\overset{!}{=}+\underset{n}{x}+\stackrel{def}{=}",
        "=^!+xₙ+=ᵈᵉᶠ",
    );
}

#[test]
fn renders_indexed_roots_and_additional_accents_and_wrappers() {
    check(
        r"\sqrt[2]{x}+\sqrt[3]{x}+\sqrt[4]{x}+\sqrt[n]{x}+\sqrt[k]{x+1}",
        "√x+∛x+∜x+ⁿ√x+ᵏ√(x+1)",
    );
    check(
        r"\acute{x}+\grave{y}+\widehat{xyz}+\overrightarrow{AB}",
        "x́+ỳ+widehat(xyz)+overrightarrow(AB)",
    );
    check(
        r"\textnormal{hello}+\mbox{world}+\boldsymbol{x}",
        "hello+world+x",
    );
}

#[test]
fn renders_additional_display_environments() {
    check(
        r"\begin{equation}\begin{split}a&=b\\&=c\end{split}\end{equation}",
        "a = b\n= c",
    );
    check(
        r"\begin{alignedat}{2}a&=b&\quad c&=d\\e&=f&g&=h\end{alignedat}",
        "a = b c = d\ne = f g = h",
    );
}

#[test]
fn uses_natural_case_conditions_and_aligns_matrix_columns() {
    check(
        r"\begin{cases}a & x<0 \\ b & \text{if }x=0 \\ c & \text{otherwise}\end{cases}",
        "⎧ a if x < 0\n⎨ b if x = 0\n⎩ c otherwise",
    );
    check(
        r"\begin{pmatrix}1&200\\3000&4\end{pmatrix}",
        "⎛ 1    │ 200 ⎞\n⎝ 3000 │ 4   ⎠",
    );
}

#[test]
fn composes_matrices_with_fractions_and_adjacent_matrices() {
    check_display(
        "R\\left(\\frac{\\pi}{4}\\right)\n=\n\\begin{pmatrix}\n\\frac{\\sqrt{2}}{2} & -\\frac{\\sqrt{2}}{2}\\\\\n\\frac{\\sqrt{2}}{2} & \\frac{\\sqrt{2}}{2}\n\\end{pmatrix}.",
        "   π\nR( ─ ) = ⎛ (√2)/2 │ -(√2)/2 ⎞\n   4     ⎝ (√2)/2 │ (√2)/2  ⎠.",
    );
    check_display(
        "\\mathbf w\n=\nR\\left(\\frac{\\pi}{4}\\right)\n\\begin{pmatrix}1\\\\0\\end{pmatrix}\n=\n\\begin{pmatrix}\\frac{\\sqrt{2}}{2}\\\\\\frac{\\sqrt{2}}{2}\\end{pmatrix}.",
        "       π\nw = R( ─ ) ⎛ 1 ⎞ = ⎛ (√2)/2 ⎞\n       4   ⎝ 0 ⎠   ⎝ (√2)/2 ⎠.",
    );
    check_display(
        "A\\mathbf e_1=\\begin{pmatrix}\\pi\\\\0\\end{pmatrix},\\qquad A\\mathbf e_2=\\begin{pmatrix}0\\\\\\frac{1}{\\pi}\\end{pmatrix}.",
        "Ae₁ = ⎛ π ⎞, Ae₂ = ⎛ 0   ⎞\n      ⎝ 0 ⎠        ⎝ 1/π ⎠.",
    );
    check_display(
        r"\sum_{i=0}^n x_i=\begin{pmatrix}a&b\\c&d\end{pmatrix}.",
        " n\n ∑  xᵢ = ⎛ a │ b ⎞\ni=0      ⎝ c │ d ⎠.",
    );
}

#[test]
fn normalizes_relation_multiplication_and_named_operator_spacing() {
    for source in ["x=y", "x =y", "x=\ny", "x\n=\ny"] {
        check(source, "x = y");
    }
    check("x_{i=0}", "xᵢ₌₀");
    check(r"x\neq0", "x ≠ 0");
    check(r"A\to B", "A → B");
    check(r"\pi\cdot\frac{1}{\pi}", "π · 1/π");
    check(r"\sin\theta", "sin θ");
    check(r"\sin^2 x", "sin² x");
    check(r"-\sin\theta", "-sin θ");
    check(r"i\sin\theta", "i sin θ");
    check(r"\det(A)", "det(A)");
}

#[test]
fn treats_a_backslash_followed_by_a_line_ending_as_control_space() {
    let source = "\\boxed{\n(1,1,1),\\ (1,1,2),\\ (1,2,5),\\ (1,5,13),\\ (2,5,29),\\\n(1,13,34),\\ (1,34,89)\n}.";
    check_display(
        source,
        "[(1,1,1), (1,1,2), (1,2,5), (1,5,13), (2,5,29), (1,13,34), (1,34,89)].",
    );
    check("a\\\r\nb", "a b");
}

#[test]
fn stacks_operator_limits_in_display_mode() {
    check_display(r"\sum_{i=0}^n x_i", " n\n ∑  xᵢ\ni=0");
    check_display(r"\min_{x\in X} f(x)", "min f(x)\nx∈X");
    check_display(
        r"\operatorname*{arg\,max}_{x\in X} f(x)",
        "arg max f(x)\n  x∈X",
    );
    check_display(r"\int\nolimits_0^1 f(x)\,dx", "∫₀¹ f(x) dx");
    check_display(r"\int\limits_0^1 f(x)\,dx", "1\n∫ f(x) dx\n0");
}

#[test]
fn uses_the_middle_brace_for_intermediate_case_rows() {
    check(
        r"\begin{cases}a & x<0 \\ b & x=0 \\ c & x>0\end{cases}",
        "⎧ a if x < 0\n⎨ b if x = 0\n⎩ c if x > 0",
    );
}

#[test]
fn stacks_fractions_in_display_mode() {
    check_display(
        r"x=\frac{-b\pm\sqrt{b^2-4ac}}{2a}",
        "    -b±√(b²-4ac)\nx = ────────────\n         2a",
    );
    check_display(r"\frac{x^2+1}{x-1}", "x²+1\n────\nx-1");
    check_display("\\frac{1}\n{2}", "1\n─\n2");
}

#[test]
fn keeps_nested_display_fractions_linear() {
    let cases = [
        (
            r"\frac{\frac{x^2+1}{x-1}-\frac{2x}{x+1}}{\frac{x}{x^2-1}}",
            "(x²+1)/(x-1)-2x/(x+1)\n─────────────────────\n      x/(x²-1)",
        ),
        (
            r"\lim_{x\to 0}\frac{\frac{\sin x}{x}-1}{\frac{e^x-1}{x}-1}=0",
            "     (sin x)/x-1\nlim  ─────────── = 0\nx→0  (eˣ-1)/x-1",
        ),
        (
            r"\frac{1+\frac{1}{1+\frac{1}{x}}}{1-\frac{1}{1-\frac{1}{x}}}",
            "1+1/(1+1/x)\n───────────\n1-1/(1-1/x)",
        ),
    ];
    for (source, expected) in cases {
        check_display(source, expected);
    }
}

#[test]
fn keeps_fractions_linear_in_scripts_and_text_style_fractions() {
    check_display(r"e^{\frac{1}{2}}", "e^(1/2)");
    check_display(r"\tfrac{1}{2}", "1/2");
}

#[test]
fn returns_none_for_unsupported_commands() {
    assert_eq!(render_latex(r"x + \unknown{y}"), None);
}

#[test]
fn returns_none_for_malformed_groups_and_environments() {
    let malformed = [r"\frac{1}{x", "x}", r"\begin{matrix}1 & 2", "x\\"];
    for source in malformed {
        assert_eq!(render_latex(source), None, "render_latex({source:?})");
    }
}
