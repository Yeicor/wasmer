(module
  (tag $e0)

  (func (export "simple-throw-catch")
    (block $h
      (try_table (catch_all $h) (throw $e0))
      (return)
    )
  )
)

(assert_return (invoke "simple-throw-catch"))
