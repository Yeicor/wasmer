(module
  (tag $e0)

  (func (export "simple-throw-catch") (result i32)
    (block $h
      (try_table (catch_all $h) (throw $e0))
	  (i32.const 0)
      (return)
    )
	(i32.const 1)
	(return)
  )
)

(assert_return (invoke "simple-throw-catch") (i32.const 0))
