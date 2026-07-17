extends Node

func _ready() -> void:
	var button := Button.new()
	button.pressed.connect(_on_pressed.bind(button))
	button.gui_input.connect(Callable(self, "_on_input"))

func _goto_map() -> void:
	GameFlow.return_to_map()
	EffectManager.apply_effect()

func _on_pressed(source) -> void:
	pass

func _on_input(event) -> void:
	pass
