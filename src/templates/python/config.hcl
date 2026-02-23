# a3s-code agent configuration
agent "{{ project_name }}" {
  model       = "claude-sonnet-4-5"
  max_turns   = 10
  system      = "You are a helpful assistant."

  skills = [
    "./skills/hello.py",
  ]
}
