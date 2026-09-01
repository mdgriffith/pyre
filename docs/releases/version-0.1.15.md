# Pyre - 0.1.15

`0.1.15` fixes Elm generation for nullable query parameters, including typed JSON parameters such as `Json<ChoiceStorage>?`. Generated Elm inputs now use `Maybe`, and `Nothing` is encoded as JSON `null`, allowing callers to query legacy rows with nullable values.
