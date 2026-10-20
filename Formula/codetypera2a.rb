class Codetypera2a < Formula
  desc "A2A hub, dispatcher and launcher for opencode v2 agents (each agent in its own tab)"
  homepage "https://git.cargdev.io/CarGDev/codetyper.a2a"
  head "https://git.cargdev.io/CarGDev/codetyper.a2a.git", branch: "master"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    # `oc` is the same binary run under another name (opencode + dispatcher launcher).
    bin.install_symlink "codetypera2a" => "oc"
  end

  def caveats
    <<~EOS
      Set up opencode (agents, commands, tabs plugin, MCP servers) once:
        codetypera2a init --shell
      then restart opencode. `oc status` shows whether the dispatcher is running.
    EOS
  end

  test do
    assert_match "codetypera2a", shell_output("#{bin}/codetypera2a help")
    assert_match "check-models", shell_output("#{bin}/oc help")
  end
end
